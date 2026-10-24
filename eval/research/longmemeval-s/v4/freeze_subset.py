#!/usr/bin/env python3
"""Freeze the preregistered strict-ID-disjoint LongMemEval_S v4 subset."""

from __future__ import annotations

import codecs
import hashlib
import json
import shutil
import urllib.request
from collections import Counter
from pathlib import Path


OUT = Path(__file__).resolve().parent
ARTIFACTS = Path("/tmp/oxibrain-longmemeval-s-public-v4")
DATASET_ID = "xiaowu0162/longmemeval-cleaned"
REVISION = "98d7416c24c778c2fee6e6f3006e7a073259d48f"
SOURCE_NAME = "longmemeval_s_cleaned.json"
SOURCE_SHA256 = "d6f21ea9d60a0d56f34a05b609c79c88a451d2ae03597821ea3d5a9678c3a442"
SOURCE_LENGTH = "277383467"
SOURCE_URL = f"https://huggingface.co/datasets/{DATASET_ID}/resolve/{REVISION}/{SOURCE_NAME}"
LICENSE_REVISION = "9e0b455f4ef0e2ab8f2e582289761153549043fc"
LICENSE_URL = f"https://raw.githubusercontent.com/xiaowu0162/LongMemEval/{LICENSE_REVISION}/LICENSE"
LICENSE_SHA256 = "d3c4b9aa54759df6ded337978a6f3b55b75615e5e4525c3b82d7e2627d4b9732"

# Fixed before source inspection. Ranks are SHA256(seed + source question ID).
V2_SEED = "oxibrain-release-0.21.0-unseen-v2"
V3_SEED = "oxibrain-release-0.21.0-unseen-v3"
V4_SEED = "oxibrain-release-0.21.0-unseen-v4"
QUESTION_TYPES = (
    "knowledge-update",
    "multi-session",
    "single-session-assistant",
    "single-session-preference",
    "single-session-user",
    "temporal-reasoning",
)
ABSTENTION_TYPES_V1 = frozenset(
    ("knowledge-update", "multi-session", "single-session-user", "temporal-reasoning")
)
PER_TYPE_QUOTA = 2
EXPECTED_ELIGIBLE_COUNTS = {
    "knowledge-update": {"answerable": 66, "abstention": 0},
    "multi-session": {"answerable": 115, "abstention": 6},
    "single-session-assistant": {"answerable": 50, "abstention": 0},
    "single-session-preference": {"answerable": 24, "abstention": 0},
    "single-session-user": {"answerable": 58, "abstention": 0},
    "temporal-reasoning": {"answerable": 121, "abstention": 0},
}

EXPECTED_V1_INPUT_SHA256 = "3d098e4e4aa0f0c2a9a6f279921b23d25ab7c81d94432e57983975a69a0d06ca"
EXPECTED_V1_GOLD_SHA256 = "a2e0f50dd2cc530454b24155d8da0701d54d4c15880400d1147f1268c1dfcf49"
EXPECTED_V1_IDS_SHA256 = "38a861cb70d53c7cff9a63642b8ba3901f43f404cd0d6e7e8d0d8079b180366c"
EXPECTED_V2_INPUT_SHA256 = "2ed7811b885afaaf04752248c8e3a9a7a4823f6eefb971ab67ab37cc085345a3"
EXPECTED_V2_GOLD_SHA256 = "d4fac57d5b98ab672e05e5ebfbf789e110e4cc480ce031135463fb7160c124d3"
EXPECTED_V2_IDS_SHA256 = "2c7767c1d4819679e97445bb193e661acf5d0b10fd492993a7447cd5f362b1b0"
EXPECTED_V3_INPUT_SHA256 = "66b0f492c3315a31dba5e6c78fe06311e09ed76f9e981406fdbfb10b6cfde06d"
EXPECTED_V3_GOLD_SHA256 = "65bc6612cd104cc7a05dbf5d02c2a773c16e53f0d71940ad6efbdc73f910b4fd"
EXPECTED_V3_IDS_SHA256 = "e643b1bfcffd6322eb43ffda3cd4beb9d3ee789d473b9b32be699fd4a3426cc2"
EXPECTED_V1_V2_IDS_SHA256 = "4b3a058ea75dea36cd4cf2db564598f856c8d214a7ebccb6edcc9d28dbc6a2ba"


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def id_set_sha256(ids: set[str]) -> str:
    return sha256("\n".join(sorted(ids)).encode("utf-8"))


def id_multiset_sha256(ids: list[str]) -> str:
    return sha256("\n".join(sorted(ids)).encode("utf-8"))


def occurrence_list_sha256(occurrences: list[tuple[int, str]]) -> str:
    data = json.dumps(sorted(occurrences), ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    return sha256(data)


def jsonl_bytes(rows: list[dict]) -> bytes:
    return "".join(
        json.dumps(row, ensure_ascii=False, sort_keys=True, separators=(",", ":")) + "\n"
        for row in rows
    ).encode("utf-8")


def write_jsonl(path: Path, rows: list[dict]) -> dict:
    data = jsonl_bytes(rows)
    path.write_bytes(data)
    return {"path": str(path), "sha256": sha256(data), "bytes": len(data), "rows": len(rows)}


def status_for(item: dict) -> str:
    return "abstention" if str(item.get("question_id", "")).endswith("_abs") else "answerable"


def prepare_rows(
    selected: list[tuple[str, str, str, int, dict]],
) -> tuple[list[dict], list[dict], dict]:
    inputs = []
    gold = []
    coverage: dict[str, dict] = {}
    for index, (question_type, status, _rank, _source_index, item) in enumerate(selected, 1):
        case_id = f"LME-S-{index:03d}"
        safe_sessions = []
        annotated_turns = []
        sessions = item.get("haystack_sessions", [])
        session_ids = item.get("haystack_session_ids", [])
        session_dates = item.get("haystack_dates", [])
        if len(sessions) != len(session_ids) or len(sessions) != len(session_dates):
            raise SystemExit("source session metadata arrays are not parallel")
        for session_index, session in enumerate(sessions):
            session_id = session_ids[session_index]
            date = session_dates[session_index]
            if isinstance(session, dict):
                session_id = session.get("session_id") or session.get("id") or session_id
                date = session.get("date") or date
                turns = session.get("turns") or session.get("dialogue") or session.get("messages") or []
            elif isinstance(session, list):
                turns = session
            else:
                raise SystemExit("source contains an unexpected session shape")
            safe_turns = []
            for turn_index, turn in enumerate(turns):
                if isinstance(turn, dict):
                    if turn.get("has_answer") is True:
                        annotated_turns.append({
                            "session_id": session_id,
                            "session_index": session_index,
                            "turn_index": turn_index,
                        })
                    safe_turns.append({
                        key: value for key, value in turn.items()
                        if key not in ("has_answer", "answer_session_ids")
                    })
                else:
                    safe_turns.append(turn)
            safe_sessions.append({"session_id": session_id, "date": date, "turns": safe_turns})

        inputs.append({
            "case_id": case_id,
            "question": item.get("question"),
            "question_date": item.get("question_date"),
            "history_sessions": safe_sessions,
        })
        gold.append({
            "case_id": case_id,
            "source_question_id": item.get("question_id"),
            "question_type": question_type,
            "abstention": status == "abstention",
            "answer": item.get("answer"),
            "answer_session_ids": item.get("answer_session_ids", []),
            "annotated_answer_turns": annotated_turns,
        })
        bucket = coverage.setdefault(question_type, {
            "answerable": 0,
            "abstention": 0,
            "sessions": 0,
            "turns": 0,
            "answer_turn_annotations": 0,
        })
        bucket[status] += 1
        bucket["sessions"] += len(safe_sessions)
        bucket["turns"] += sum(len(session["turns"]) for session in safe_sessions)
        bucket["answer_turn_annotations"] += len(annotated_turns)
    return inputs, gold, coverage


def rank_candidate(
    ranked: dict[tuple[str, str], list[tuple[str, str, int, dict]]],
    key: tuple[str, str],
    seed: str,
    source_index: int,
    item: dict,
    limit: int,
) -> None:
    question_id = str(item.get("question_id", ""))
    if not question_id:
        raise SystemExit("pinned source contains a record without a question ID")
    digest = sha256((seed + question_id).encode("utf-8"))
    pool = ranked.setdefault(key, [])
    pool.append((digest, question_id, source_index, item))
    pool.sort(key=lambda candidate: (candidate[0], candidate[1], candidate[2]))
    del pool[limit:]


def select_ranked(
    ranked: dict[tuple[str, str], list[tuple[str, str, int, dict]]],
    expected_keys: set[tuple[str, str]],
    excluded_ids: set[str] | None = None,
    label: str = "selection",
    expected_count: int = 20,
) -> list[tuple[str, str, str, int, dict]]:
    blocked = excluded_ids or set()
    selected = []
    for key in sorted(expected_keys):
        pool = [candidate for candidate in ranked.get(key, []) if candidate[1] not in blocked]
        pool = pool[:PER_TYPE_QUOTA]
        if len(pool) != PER_TYPE_QUOTA:
            raise SystemExit(f"{label} candidate pool does not satisfy the frozen category quotas")
        question_type, status = key
        selected.extend(
            (question_type, status, digest, source_index, item)
            for digest, _question_id, source_index, item in pool
        )
    selected.sort(key=lambda row: (
        row[0], row[1], row[2], str(row[4].get("question_id", "")), row[3]
    ))
    if len(selected) != expected_count:
        raise SystemExit(f"{label} occurrence count verification failed")
    return selected


def scan_source(consume) -> tuple[int, dict[str, int], int]:
    decoder = json.JSONDecoder()
    utf8 = codecs.getincrementaldecoder("utf-8")()
    raw_hash = hashlib.sha256()
    buffer = ""
    position = 0
    started = False
    scanned = 0
    type_counts: dict[str, int] = {}
    seen_ids: set[str] = set()
    duplicate_id_occurrences = 0

    request = urllib.request.Request(SOURCE_URL, headers={"User-Agent": "oxibrain-research/1.0"})
    with urllib.request.urlopen(request, timeout=180) as response:
        if response.status != 200 or response.headers.get("Content-Length") != SOURCE_LENGTH:
            raise SystemExit("pinned source response verification failed")
        while True:
            chunk = response.read(1024 * 1024)
            if not chunk:
                break
            raw_hash.update(chunk)
            buffer += utf8.decode(chunk)
            if not started:
                start = buffer.find("[")
                if start < 0:
                    continue
                buffer = buffer[start + 1:]
                position = 0
                started = True
            while True:
                while position < len(buffer) and buffer[position] in " \t\r\n,":
                    position += 1
                if position >= len(buffer) or buffer[position] == "]":
                    break
                try:
                    item, end = decoder.raw_decode(buffer, position)
                except json.JSONDecodeError:
                    break
                question_id = str(item.get("question_id", ""))
                if not question_id:
                    raise SystemExit("pinned source contains a record without a question ID")
                if question_id in seen_ids:
                    duplicate_id_occurrences += 1
                else:
                    seen_ids.add(question_id)
                question_type = str(item.get("question_type", "unknown"))
                type_counts[question_type] = type_counts.get(question_type, 0) + 1
                consume(item, scanned)
                scanned += 1
                position = end
            if position > 1024 * 1024:
                buffer = buffer[position:]
                position = 0
        buffer += utf8.decode(b"", final=True)
        while True:
            while position < len(buffer) and buffer[position] in " \t\r\n,":
                position += 1
            if position >= len(buffer) or buffer[position] == "]":
                break
            item, end = decoder.raw_decode(buffer, position)
            question_id = str(item.get("question_id", ""))
            if not question_id:
                raise SystemExit("pinned source contains a record without a question ID")
            if question_id in seen_ids:
                duplicate_id_occurrences += 1
            else:
                seen_ids.add(question_id)
            question_type = str(item.get("question_type", "unknown"))
            type_counts[question_type] = type_counts.get(question_type, 0) + 1
            consume(item, scanned)
            scanned += 1
            position = end

    if raw_hash.hexdigest() != SOURCE_SHA256 or scanned != 500:
        raise SystemExit("pinned source content verification failed")
    return scanned, type_counts, duplicate_id_occurrences


def verify_source_metadata() -> None:
    api_url = f"https://huggingface.co/api/datasets/{DATASET_ID}"
    with urllib.request.urlopen(api_url, timeout=30) as response:
        api = json.load(response)
    if api.get("sha") != REVISION:
        raise SystemExit("pinned dataset revision verification failed")
    if api.get("cardData", {}).get("license") != "mit":
        raise SystemExit("pinned dataset license metadata verification failed")


def main() -> None:
    verify_source_metadata()
    expected_prior_keys = {
        (question_type, "answerable") for question_type in QUESTION_TYPES
    } | {
        (question_type, "abstention") for question_type in ABSTENTION_TYPES_V1
    }
    expected_answerable_keys = {(question_type, "answerable") for question_type in QUESTION_TYPES}

    v1_first: dict[tuple[str, str], list[tuple[int, dict]]] = {}
    v1_id_list: list[str] = []
    v2_ranked: dict[tuple[str, str], list[tuple[str, str, int, dict]]] = {}
    v3_ranked: dict[tuple[str, str], list[tuple[str, str, int, dict]]] = {}
    all_source_counts: Counter[tuple[str, str]] = Counter()

    def consume_prior(item: dict, source_index: int) -> None:
        question_type = str(item.get("question_type", "unknown"))
        question_id = str(item.get("question_id", ""))
        status = status_for(item)
        key = (question_type, status)
        all_source_counts[key] += 1
        first = v1_first.setdefault(key, [])
        if len(first) < PER_TYPE_QUOTA:
            first.append((source_index, item))
            v1_id_list.append(question_id)
            return
        rank_candidate(v2_ranked, key, V2_SEED, source_index, item, PER_TYPE_QUOTA)
        rank_candidate(v3_ranked, key, V3_SEED, source_index, item, PER_TYPE_QUOTA * 2)

    scanned, type_counts, duplicate_id_occurrences = scan_source(consume_prior)
    if set(type_counts) != set(QUESTION_TYPES):
        raise SystemExit("pinned source question-type profile changed")

    v1_selected = []
    for (question_type, status), rows in v1_first.items():
        v1_selected.extend(
            (question_type, status, "", source_index, item)
            for source_index, item in rows
        )
    v1_selected.sort(key=lambda row: (
        row[0], row[1], str(row[4].get("question_id", "")), row[3]
    ))
    if len(v1_selected) != 20:
        raise SystemExit("reconstructed v1 occurrence count verification failed")
    v1_inputs, v1_gold, _ = prepare_rows(v1_selected)
    v1_ids = set(v1_id_list)
    if sha256(jsonl_bytes(v1_inputs)) != EXPECTED_V1_INPUT_SHA256:
        raise SystemExit("reconstructed v1 input does not match its frozen hash")
    if sha256(jsonl_bytes(v1_gold)) != EXPECTED_V1_GOLD_SHA256 or id_set_sha256(v1_ids) != EXPECTED_V1_IDS_SHA256:
        raise SystemExit("reconstructed v1 gold or selected-ID hash does not match")

    v2_selected = select_ranked(v2_ranked, expected_prior_keys, label="v2 reconstruction")
    v2_inputs, v2_gold, _ = prepare_rows(v2_selected)
    v2_ids = {str(row[4].get("question_id", "")) for row in v2_selected}
    if v2_ids & v1_ids:
        raise SystemExit("reconstructed v2 selection overlaps v1")
    if sha256(jsonl_bytes(v2_inputs)) != EXPECTED_V2_INPUT_SHA256:
        raise SystemExit("reconstructed v2 input does not match its frozen hash")
    if sha256(jsonl_bytes(v2_gold)) != EXPECTED_V2_GOLD_SHA256 or id_set_sha256(v2_ids) != EXPECTED_V2_IDS_SHA256:
        raise SystemExit("reconstructed v2 gold or selected-ID hash does not match")

    v3_selected = select_ranked(v3_ranked, expected_prior_keys, v2_ids, label="v3 reconstruction")
    v3_inputs, v3_gold, _ = prepare_rows(v3_selected)
    v3_ids = {str(row[4].get("question_id", "")) for row in v3_selected}
    v1_v2_ids = v1_ids | v2_ids
    if v3_ids & v1_v2_ids:
        raise SystemExit("reconstructed v3 selection overlaps v1 or v2")
    if sha256(jsonl_bytes(v3_inputs)) != EXPECTED_V3_INPUT_SHA256:
        raise SystemExit("reconstructed v3 input does not match its frozen hash")
    if sha256(jsonl_bytes(v3_gold)) != EXPECTED_V3_GOLD_SHA256 or id_set_sha256(v3_ids) != EXPECTED_V3_IDS_SHA256:
        raise SystemExit("reconstructed v3 gold or selected-ID hash does not match")
    if id_set_sha256(v1_v2_ids) != EXPECTED_V1_V2_IDS_SHA256:
        raise SystemExit("reconstructed v1/v2 exclusion hash does not match")

    prior_rows = v1_selected + v2_selected + v3_selected
    prior_occurrences = [(row[3], str(row[4].get("question_id", ""))) for row in prior_rows]
    prior_ids = v1_ids | v2_ids | v3_ids
    if len(prior_rows) != 60 or len({row[0] for row in prior_occurrences}) != 60:
        raise SystemExit("prior selected source occurrence count verification failed")
    if len(prior_ids) != 60:
        raise SystemExit("prior selected question-ID set verification failed")

    # The first pass has aggregates by category/status but not per ID. Re-scan once
    # to calculate strict ID-disjoint eligibility and construct the v4 candidates.
    answerable_ranked: dict[tuple[str, str], list[tuple[str, str, int, dict]]] = {}
    remaining_abstentions: list[tuple[str, str, int, dict]] = []
    eligible_occurrence_counts: Counter[tuple[str, str]] = Counter()

    def consume_v4(item: dict, source_index: int) -> None:
        question_type = str(item.get("question_type", "unknown"))
        question_id = str(item.get("question_id", ""))
        status = status_for(item)
        if question_id in prior_ids:
            return
        eligible_occurrence_counts[(question_type, status)] += 1
        key = (question_type, status)
        rank_candidate(answerable_ranked, key, V4_SEED, source_index, item, PER_TYPE_QUOTA)
        if status == "abstention" and question_type == "multi-session":
            digest = sha256((V4_SEED + question_id).encode("utf-8"))
            remaining_abstentions.append((digest, question_id, source_index, item))

    second_scanned, second_type_counts, second_duplicate_id_occurrences = scan_source(consume_v4)
    if (second_scanned, second_type_counts, second_duplicate_id_occurrences) != (
        scanned, type_counts, duplicate_id_occurrences
    ):
        raise SystemExit("pinned source changed between prior-set reconstruction and v4 selection")

    eligible_profile = {
        question_type: {
            "answerable": eligible_occurrence_counts[(question_type, "answerable")],
            "abstention": eligible_occurrence_counts[(question_type, "abstention")],
        }
        for question_type in QUESTION_TYPES
    }
    if eligible_profile != EXPECTED_ELIGIBLE_COUNTS:
        raise SystemExit("strict-ID eligible category counts changed from preregistered values")
    if len(remaining_abstentions) != 6:
        raise SystemExit("expected all six eligible multi-session abstentions")
    if any(eligible_occurrence_counts[(question_type, "abstention")] for question_type in QUESTION_TYPES if question_type != "multi-session"):
        raise SystemExit("unexpected eligible abstentions outside multi-session")

    answerable_selected = select_ranked(
        answerable_ranked,
        expected_answerable_keys,
        label="v4 answerable selection",
        expected_count=12,
    )
    abstention_selected = [
        ("multi-session", "abstention", digest, source_index, item)
        for digest, _question_id, source_index, item in remaining_abstentions
    ]
    abstention_selected.sort(key=lambda row: (
        row[0], row[1], row[2], str(row[4].get("question_id", "")), row[3]
    ))
    selected = answerable_selected + abstention_selected
    selected.sort(key=lambda row: (
        row[0], row[1], row[2], str(row[4].get("question_id", "")), row[3]
    ))
    if len(selected) != 18:
        raise SystemExit("preregistered v4 case count verification failed")

    inputs, gold, coverage = prepare_rows(selected)
    selected_id_list = [str(row[4].get("question_id", "")) for row in selected]
    selected_id_set = set(selected_id_list)
    selected_occurrences = [(row[3], str(row[4].get("question_id", ""))) for row in selected]
    if selected_id_set & prior_ids:
        raise SystemExit("v4 selection is not disjoint from prior selected question IDs")
    if len({row[0] for row in selected_occurrences}) != 18:
        raise SystemExit("v4 selection contains a repeated source occurrence")
    total_answerable = sum(bucket["answerable"] for bucket in coverage.values())
    total_abstention = sum(bucket["abstention"] for bucket in coverage.values())
    if (total_answerable, total_abstention) != (12, 6):
        raise SystemExit("preregistered v4 answerability quota verification failed")

    source_category_coverage = {}
    for question_type in QUESTION_TYPES:
        source_category_coverage[question_type] = {
            "source_answerable": all_source_counts[(question_type, "answerable")],
            "source_abstention": all_source_counts[(question_type, "abstention")],
            "eligible_answerable": eligible_profile[question_type]["answerable"],
            "eligible_abstention": eligible_profile[question_type]["abstention"],
            "selected_answerable": coverage.get(question_type, {}).get("answerable", 0),
            "selected_abstention": coverage.get(question_type, {}).get("abstention", 0),
        }

    ARTIFACTS.mkdir(parents=True, exist_ok=True)
    input_info = write_jsonl(ARTIFACTS / "input.jsonl", inputs)
    gold_info = write_jsonl(ARTIFACTS / "gold.jsonl", gold)

    license_request = urllib.request.Request(LICENSE_URL, headers={"User-Agent": "oxibrain-research/1.0"})
    with urllib.request.urlopen(license_request, timeout=30) as response:
        license_bytes = response.read()
    if sha256(license_bytes) != LICENSE_SHA256:
        raise SystemExit("pinned upstream license checksum verification failed")
    (OUT / "LICENSE.txt").write_bytes(license_bytes)

    profile = {
        "cases": len(selected),
        "answerable": total_answerable,
        "abstention": total_abstention,
        "sessions": sum(bucket["sessions"] for bucket in coverage.values()),
        "turns": sum(bucket["turns"] for bucket in coverage.values()),
        "answer_turn_annotations": sum(
            bucket["answer_turn_annotations"] for bucket in coverage.values()
        ),
        "selected_unique_source_question_ids": len(selected_id_set),
        "selected_duplicate_id_occurrences": len(selected_id_list) - len(selected_id_set),
    }
    prior_validation = {
        "v1": {
            "occurrences": len(v1_selected),
            "input_sha256": EXPECTED_V1_INPUT_SHA256,
            "gold_sha256": EXPECTED_V1_GOLD_SHA256,
            "selected_id_set_sha256": id_set_sha256(v1_ids),
        },
        "v2": {
            "occurrences": len(v2_selected),
            "input_sha256": EXPECTED_V2_INPUT_SHA256,
            "gold_sha256": EXPECTED_V2_GOLD_SHA256,
            "selected_id_set_sha256": id_set_sha256(v2_ids),
        },
        "v3": {
            "occurrences": len(v3_selected),
            "input_sha256": EXPECTED_V3_INPUT_SHA256,
            "gold_sha256": EXPECTED_V3_GOLD_SHA256,
            "selected_id_set_sha256": id_set_sha256(v3_ids),
        },
    }
    manifest = {
        "schema": "oxibrain-external-memory-qa/1",
        "dataset": "LongMemEval cleaned LongMemEval_S",
        "paper": "https://arxiv.org/abs/2410.10813v2",
        "official_repo": "https://github.com/xiaowu0162/LongMemEval",
        "official_dataset": f"https://huggingface.co/datasets/{DATASET_ID}",
        "revision": REVISION,
        "license": "MIT (pinned Hugging Face card metadata; upstream repository LICENSE retained verbatim)",
        "license_source": LICENSE_URL,
        "license_sha256": LICENSE_SHA256,
        "upstream_file": SOURCE_NAME,
        "upstream_file_url": SOURCE_URL,
        "upstream_file_sha256": SOURCE_SHA256,
        "source_rows_scanned_per_pass": scanned,
        "source_duplicate_id_occurrences": duplicate_id_occurrences,
        "question_type_counts": type_counts,
        "selection_seed": V4_SEED,
        "selection": (
            "V1, v2, and v3 are reconstructed from the pinned source order and their frozen "
            "input, gold, and selected-ID hashes are verified before use. Every source record "
            "whose question ID matches any of the 60 prior selected IDs is excluded. Among "
            "remaining occurrences, v4 selects the two lowest SHA256(seed + source_question_id) "
            "ranks for answerable questions in each of six types. All six remaining eligible "
            "abstentions are included; they are all multi-session. Duplicate source occurrences "
            "are preserved as separate candidates and rank ties use pinned source occurrence "
            "order. Output is sorted by type, status, rank, source ID, then occurrence index. "
            "Session IDs and dates remain zipped by index with sessions. Input excludes answer "
            "and evidence markers, source question IDs, and gold labels; gold is evaluator-only."
        ),
        "stratification_deviation": (
            "Strict exclusion of all v1/v2/v3 selected question IDs leaves six eligible "
            "abstentions, all in multi-session, and none in the other five categories. V4 "
            "therefore contains 12 answerable cases (two in each category) and all six "
            "eligible multi-session abstentions, for 18 cases total. No replacement "
            "answerables or prior IDs are used."
        ),
        "evaluation_claims": {
            "reader_score": False,
            "abstention_success_score": False,
            "note": "This 18-case subset does not claim reader or abstention-success performance.",
        },
        "prior_selected_occurrences": 60,
        "prior_unique_source_question_ids": len(prior_ids),
        "prior_selected_id_set_sha256": id_set_sha256(prior_ids),
        "prior_selected_occurrences_sha256": occurrence_list_sha256(prior_occurrences),
        "prior_validation": prior_validation,
        "eligible_counts_after_strict_id_exclusion": eligible_profile,
        "source_category_coverage": source_category_coverage,
        "selected_source_question_ids_set_sha256": id_set_sha256(selected_id_set),
        "selected_source_question_ids_multiset_sha256": id_multiset_sha256(selected_id_list),
        "selected_source_occurrences_sha256": occurrence_list_sha256(selected_occurrences),
        "selected_count": len(selected),
        "selected_profile": profile,
        "selected_coverage": {key: coverage[key] for key in sorted(coverage)},
        "input": input_info,
        "gold": {**gold_info, "audience": "evaluator-only"},
        "preparation_script": {
            "path": "freeze_subset.py",
            "sha256": sha256(Path(__file__).read_bytes()),
        },
        "annotation_caveat": "Human-curated questions and evidence labels; evidence sessions were LLM-simulated and manually screened/edited per the paper. English only; not real-user dialogue and not an independently collected multilingual benchmark.",
        "retrieval_caveat": "The S track keeps distractor histories; answer markers are present only in gold.jsonl. Requires a separate ingestion/evaluation adapter. Does not test cross-writing-system parity.",
    }
    manifest_bytes = (json.dumps(manifest, ensure_ascii=False, sort_keys=True, indent=2) + "\n").encode("utf-8")
    (OUT / "manifest.json").write_bytes(manifest_bytes)
    if OUT.resolve() != ARTIFACTS.resolve():
        for filename in ("freeze_subset.py", "manifest.json", "LICENSE.txt"):
            shutil.copyfile(OUT / filename, ARTIFACTS / filename)

    print(json.dumps({
        "source_sha256": SOURCE_SHA256,
        "source_rows_scanned_per_pass": scanned,
        "source_duplicate_id_occurrences": duplicate_id_occurrences,
        "prior_selected_occurrences": 60,
        "prior_unique_source_question_ids": len(prior_ids),
        "prior_selected_id_set_sha256": id_set_sha256(prior_ids),
        "prior_selected_occurrences_sha256": occurrence_list_sha256(prior_occurrences),
        "prior_validation": prior_validation,
        "eligible_counts_after_strict_id_exclusion": eligible_profile,
        "source_category_coverage": source_category_coverage,
        "selected_source_question_ids_set_sha256": id_set_sha256(selected_id_set),
        "selected_source_question_ids_multiset_sha256": id_multiset_sha256(selected_id_list),
        "selected_source_occurrences_sha256": occurrence_list_sha256(selected_occurrences),
        "selected_profile": profile,
        "input": input_info,
        "gold": {**gold_info, "audience": "evaluator-only"},
        "script_sha256": sha256(Path(__file__).read_bytes()),
        "manifest_sha256": sha256(manifest_bytes),
    }, ensure_ascii=False, sort_keys=True, indent=2))


if __name__ == "__main__":
    main()
