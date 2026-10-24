#!/usr/bin/env python3
"""Freeze a deterministic, label-separated LongMemEval_S release subset."""

import codecs
import hashlib
import json
import urllib.request
from pathlib import Path


OUT = Path(__file__).resolve().parent
ARTIFACTS = Path("/tmp/oxibrain-longmemeval-s-public-v2")
DATASET_ID = "xiaowu0162/longmemeval-cleaned"
REVISION = "98d7416c24c778c2fee6e6f3006e7a073259d48f"
SOURCE_NAME = "longmemeval_s_cleaned.json"
SOURCE_SHA256 = "d6f21ea9d60a0d56f34a05b609c79c88a451d2ae03597821ea3d5a9678c3a442"
SOURCE_LENGTH = "277383467"
SOURCE_URL = f"https://huggingface.co/datasets/{DATASET_ID}/resolve/{REVISION}/{SOURCE_NAME}"
LICENSE_REVISION = "9e0b455f4ef0e2ab8f2e582289761153549043fc"
LICENSE_URL = f"https://raw.githubusercontent.com/xiaowu0162/LongMemEval/{LICENSE_REVISION}/LICENSE"
LICENSE_SHA256 = "d3c4b9aa54759df6ded337978a6f3b55b75615e5e4525c3b82d7e2627d4b9732"

# Fixed before reading the pinned source. Ranks are SHA256(seed + source question ID).
SEED = "oxibrain-release-0.21.0-unseen-v2"
QUESTION_TYPES = (
    "knowledge-update",
    "multi-session",
    "single-session-assistant",
    "single-session-preference",
    "single-session-user",
    "temporal-reasoning",
)
ABSTENTION_TYPES = frozenset(
    ("knowledge-update", "multi-session", "single-session-user", "temporal-reasoning")
)
PER_TYPE_QUOTA = 2
EXPECTED_V1_INPUT_SHA256 = "3d098e4e4aa0f0c2a9a6f279921b23d25ab7c81d94432e57983975a69a0d06ca"
EXPECTED_V1_GOLD_SHA256 = "a2e0f50dd2cc530454b24155d8da0701d54d4c15880400d1147f1268c1dfcf49"


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


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


def prepare_rows(selected: list[tuple[str, str, str, dict]]) -> tuple[list[dict], list[dict], dict]:
    inputs = []
    gold = []
    coverage: dict[str, dict] = {}
    for index, (question_type, status, _rank, item) in enumerate(selected, 1):
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


def main() -> None:
    api_url = f"https://huggingface.co/api/datasets/{DATASET_ID}"
    with urllib.request.urlopen(api_url, timeout=30) as response:
        api = json.load(response)
    if api.get("sha") != REVISION:
        raise SystemExit("pinned dataset revision verification failed")
    if api.get("cardData", {}).get("license") != "mit":
        raise SystemExit("pinned dataset license metadata verification failed")

    decoder = json.JSONDecoder()
    utf8 = codecs.getincrementaldecoder("utf-8")()
    raw_hash = hashlib.sha256()
    buffer = ""
    position = 0
    started = False
    scanned = 0
    type_counts: dict[str, int] = {}
    v1_first: dict[tuple[str, str], list[dict]] = {}
    v1_ids: set[str] = set()
    v1_counts: dict[tuple[str, str], int] = {}
    ranked: dict[tuple[str, str], list[tuple[str, str, dict]]] = {}

    def consume(item: dict) -> None:
        nonlocal scanned
        scanned += 1
        question_type = str(item.get("question_type", "unknown"))
        question_id = str(item.get("question_id", ""))
        if not question_id:
            raise SystemExit("pinned source contains a record without a question ID")
        status = status_for(item)
        type_counts[question_type] = type_counts.get(question_type, 0) + 1
        key = (question_type, status)
        first = v1_first.setdefault(key, [])
        if len(first) < PER_TYPE_QUOTA:
            first.append(item)
            v1_ids.add(question_id)
            v1_counts[key] = len(first)
            return

        digest = sha256((SEED + question_id).encode("utf-8"))
        pool = ranked.setdefault(key, [])
        pool.append((digest, question_id, item))
        pool.sort(key=lambda candidate: (candidate[0], candidate[1]))
        del pool[PER_TYPE_QUOTA:]

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
                consume(item)
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
            consume(item)
            position = end

    if raw_hash.hexdigest() != SOURCE_SHA256 or scanned != 500:
        raise SystemExit("pinned source content verification failed")

    v1_selected = []
    for (question_type, status), items in v1_first.items():
        v1_selected.extend((question_type, status, "", item) for item in items)
    v1_selected.sort(key=lambda row: (row[0], row[1], str(row[3].get("question_id", ""))))
    old_inputs, old_gold, _ = prepare_rows(v1_selected)
    if sha256(jsonl_bytes(old_inputs)) != EXPECTED_V1_INPUT_SHA256:
        raise SystemExit("reconstructed v1 input does not match the frozen manifest")
    if sha256(jsonl_bytes(old_gold)) != EXPECTED_V1_GOLD_SHA256 or len(v1_ids) != 20:
        raise SystemExit("reconstructed v1 gold does not match the frozen manifest")

    expected_keys = {
        (question_type, "answerable") for question_type in QUESTION_TYPES
    } | {
        (question_type, "abstention") for question_type in ABSTENTION_TYPES
    }
    if set(type_counts) != set(QUESTION_TYPES):
        raise SystemExit("pinned source question-type profile changed")
    if any(v1_counts.get(key, 0) != PER_TYPE_QUOTA for key in expected_keys):
        raise SystemExit("v1 exclusion set does not satisfy the frozen category quotas")

    selected = []
    for key in sorted(expected_keys):
        pool = ranked.get(key, [])
        if len(pool) != PER_TYPE_QUOTA:
            raise SystemExit("v2 candidate pool does not satisfy the frozen category quotas")
        question_type, status = key
        selected.extend((question_type, status, digest, item) for digest, _question_id, item in pool)
    selected.sort(key=lambda row: (row[0], row[1], row[2], str(row[3].get("question_id", ""))))
    selected_ids = {str(row[3].get("question_id", "")) for row in selected}
    if len(selected) != 20 or selected_ids & v1_ids:
        raise SystemExit("v2 selection is not disjoint from the frozen v1 subset")

    inputs, gold, coverage = prepare_rows(selected)
    if len(inputs) != 20 or len(gold) != 20:
        raise SystemExit("frozen v2 row count verification failed")
    total_answerable = sum(row["answerable"] for row in coverage.values())
    total_abstention = sum(row["abstention"] for row in coverage.values())
    if total_answerable != 12 or total_abstention != 8:
        raise SystemExit("frozen v2 status quota verification failed")

    ARTIFACTS.mkdir(parents=True, exist_ok=True)
    input_info = write_jsonl(ARTIFACTS / "input.jsonl", inputs)
    gold_info = write_jsonl(ARTIFACTS / "gold.jsonl", gold)

    license_request = urllib.request.Request(LICENSE_URL, headers={"User-Agent": "oxibrain-research/1.0"})
    with urllib.request.urlopen(license_request, timeout=30) as response:
        license_bytes = response.read()
    if sha256(license_bytes) != LICENSE_SHA256:
        raise SystemExit("pinned upstream license checksum verification failed")
    (OUT / "LICENSE.txt").write_bytes(license_bytes)

    selected_coverage = {
        question_type: coverage[question_type]
        for question_type in sorted(coverage)
    }
    profile = {
        "cases": len(selected),
        "answerable": total_answerable,
        "abstention": total_abstention,
        "sessions": sum(bucket["sessions"] for bucket in coverage.values()),
        "turns": sum(bucket["turns"] for bucket in coverage.values()),
        "answer_turn_annotations": sum(
            bucket["answer_turn_annotations"] for bucket in coverage.values()
        ),
    }
    excluded_digest = sha256("\n".join(sorted(v1_ids)).encode("utf-8"))
    selected_digest = sha256("\n".join(sorted(selected_ids)).encode("utf-8"))
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
        "source_rows_scanned": scanned,
        "question_type_counts": type_counts,
        "selection_seed": SEED,
        "selection": (
            "The v1 exclusion set is reconstructed from the pinned v1 rule (first two "
            "answerable and first two abstention records per type where available) and "
            "verified against the frozen v1 input and gold hashes. For v2, the two "
            "lowest SHA256(seed + source_question_id) ranks are selected among remaining "
            "records for each v1 category quota: two answerable in each of six types and "
            "two abstentions in the four types that had abstentions in v1. Rank ties use "
            "source question ID. Output case IDs are assigned after sorting by type, "
            "status (abstention before answerable), then rank and source ID. Session IDs "
            "and dates remain zipped by index with sessions. Input excludes has_answer, "
            "answer_session_ids, source question IDs, and gold labels; gold is a separate "
            "evaluator-only file."
        ),
        "v1_exclusion_count": len(v1_ids),
        "v1_exclusion_ids_sha256": excluded_digest,
        "selected_source_question_ids_sha256": selected_digest,
        "selected_count": len(selected),
        "selected_profile": profile,
        "selected_coverage": selected_coverage,
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
    print(json.dumps({
        "source_sha256": SOURCE_SHA256,
        "source_rows_scanned": scanned,
        "v1_exclusion_count": len(v1_ids),
        "v1_exclusion_ids_sha256": excluded_digest,
        "selected_source_question_ids_sha256": selected_digest,
        "selected_profile": profile,
        "selected_coverage": selected_coverage,
        "input": input_info,
        "gold": {**gold_info, "audience": "evaluator-only"},
        "script_sha256": sha256(Path(__file__).read_bytes()),
        "manifest_sha256": sha256(manifest_bytes),
    }, ensure_ascii=False, sort_keys=True, indent=2))


if __name__ == "__main__":
    main()
