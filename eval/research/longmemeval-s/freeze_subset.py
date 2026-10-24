#!/usr/bin/env python3
"""Freeze a small, deterministic, label-separated LongMemEval_S subset."""
import codecs
import hashlib
import json
import urllib.request
from pathlib import Path

OUT = Path(__file__).resolve().parent
DATASET_ID = "xiaowu0162/longmemeval-cleaned"
REVISION = "98d7416c24c778c2fee6e6f3006e7a073259d48f"
SOURCE_NAME = "longmemeval_s_cleaned.json"
SOURCE_SHA256 = "d6f21ea9d60a0d56f34a05b609c79c88a451d2ae03597821ea3d5a9678c3a442"
SOURCE_URL = f"https://huggingface.co/datasets/{DATASET_ID}/resolve/{REVISION}/{SOURCE_NAME}"
LICENSE_REVISION = "9e0b455f4ef0e2ab8f2e582289761153549043fc"
LICENSE_URL = f"https://raw.githubusercontent.com/xiaowu0162/LongMemEval/{LICENSE_REVISION}/LICENSE"
LICENSE_SHA256 = "d3c4b9aa54759df6ded337978a6f3b55b75615e5e4525c3b82d7e2627d4b9732"


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def write_jsonl(path: Path, rows: list[dict]) -> dict:
    data = "".join(
        json.dumps(row, ensure_ascii=False, sort_keys=True, separators=(",", ":")) + "\n"
        for row in rows
    ).encode("utf-8")
    path.write_bytes(data)
    return {"path": path.name, "sha256": sha256(data), "bytes": len(data), "rows": len(rows)}


def main() -> None:
    api_url = f"https://huggingface.co/api/datasets/{DATASET_ID}"
    with urllib.request.urlopen(api_url, timeout=30) as response:
        api = json.load(response)
    if api.get("sha") != REVISION:
        raise SystemExit(f"dataset revision changed: expected {REVISION}, got {api.get('sha')}")
    if api.get("cardData", {}).get("license") != "mit":
        raise SystemExit("the current pinned dataset metadata no longer declares MIT")

    decoder = json.JSONDecoder()
    utf8 = codecs.getincrementaldecoder("utf-8")()
    raw_hash = hashlib.sha256()
    buffer = ""
    position = 0
    started = False
    scanned = 0
    type_counts: dict[str, int] = {}
    positives: dict[str, list[dict]] = {}
    abstentions: dict[str, list[dict]] = {}

    request = urllib.request.Request(SOURCE_URL, headers={"User-Agent": "oxibrain-research/1.0"})
    with urllib.request.urlopen(request, timeout=180) as response:
        if response.status != 200:
            raise SystemExit(f"unexpected source response status: {response.status}")
        if response.headers.get("Content-Length") != "277383467":
            raise SystemExit(f"unexpected source length: {response.headers.get('Content-Length')}")
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
                buffer = buffer[start + 1 :]
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
                scanned += 1
                question_type = str(item.get("question_type", "unknown"))
                type_counts[question_type] = type_counts.get(question_type, 0) + 1
                is_abstention = str(item.get("question_id", "")).endswith("_abs")
                pool = abstentions if is_abstention else positives
                rows = pool.setdefault(question_type, [])
                if len(rows) < 2:
                    rows.append(item)
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
            scanned += 1
            question_type = str(item.get("question_type", "unknown"))
            type_counts[question_type] = type_counts.get(question_type, 0) + 1
            is_abstention = str(item.get("question_id", "")).endswith("_abs")
            pool = abstentions if is_abstention else positives
            rows = pool.setdefault(question_type, [])
            if len(rows) < 2:
                rows.append(item)
            position = end

    actual_source_sha = raw_hash.hexdigest()
    if actual_source_sha != SOURCE_SHA256:
        raise SystemExit(f"source hash mismatch: expected {SOURCE_SHA256}, got {actual_source_sha}")
    if scanned != 500:
        raise SystemExit(f"expected 500 upstream records, scanned {scanned}")

    # Deterministic sample: first two answerable and first two abstention records
    # per type in pinned upstream order; stable output ordering by type, status,
    # and upstream question ID. Case IDs do not disclose upstream abstention IDs.
    selected = []
    for question_type in sorted(type_counts):
        for pool, status in ((abstentions, "abstention"), (positives, "answerable")):
            selected.extend((question_type, status, item) for item in pool.get(question_type, []))
    selected.sort(key=lambda row: (row[0], row[1], str(row[2].get("question_id", ""))))

    inputs = []
    gold = []
    coverage: dict[str, dict] = {}
    for index, (question_type, status, item) in enumerate(selected, 1):
        case_id = f"LME-S-{index:03d}"
        safe_sessions = []
        annotated_turns = []
        sessions = item.get("haystack_sessions", [])
        session_ids = item.get("haystack_session_ids", [])
        session_dates = item.get("haystack_dates", [])
        if len(sessions) != len(session_ids) or len(sessions) != len(session_dates):
            raise SystemExit(
                f"session metadata mismatch for {item.get('question_id')}: "
                f"sessions={len(sessions)} ids={len(session_ids)} dates={len(session_dates)}"
            )
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
                raise SystemExit(f"unexpected session type: {type(session).__name__}")
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
            "answerable": 0, "abstention": 0, "sessions": 0,
            "turns": 0, "answer_turn_annotations": 0,
        })
        bucket[status] += 1
        bucket["sessions"] += len(safe_sessions)
        bucket["turns"] += sum(len(session["turns"]) for session in safe_sessions)
        bucket["answer_turn_annotations"] += len(annotated_turns)

    input_info = write_jsonl(OUT / "input.jsonl", inputs)
    gold_info = write_jsonl(OUT / "gold.jsonl", gold)

    license_request = urllib.request.Request(LICENSE_URL, headers={"User-Agent": "oxibrain-research/1.0"})
    with urllib.request.urlopen(license_request, timeout=30) as response:
        license_bytes = response.read()
    if sha256(license_bytes) != LICENSE_SHA256:
        raise SystemExit("upstream license checksum changed")
    (OUT / "LICENSE.txt").write_bytes(license_bytes)

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
        "upstream_file_sha256": actual_source_sha,
        "source_rows_scanned": scanned,
        "question_type_counts": type_counts,
        "selection": "Pinned upstream file order: first two answerable and first two abstention instances per question_type where available; output sorted by (question_type, status [abstention before answerable], source_question_id). Local LME-S-NNN IDs are assigned after sorting. Original haystack_session_ids and haystack_dates arrays are zipped by index with haystack_sessions. All answer/evidence markers and upstream question IDs are excluded from input.jsonl; gold.jsonl is a separate evaluator-only file.",
        "selected_count": len(selected),
        "selected_coverage": coverage,
        "input": input_info,
        "gold": gold_info,
        "preparation_script": {
            "path": "freeze_subset.py",
            "sha256": sha256(Path(__file__).read_bytes()),
        },
        "annotation_caveat": "Human-curated questions and evidence labels; evidence sessions were LLM-simulated and manually screened/edited per the paper. English only; not real-user dialogue and not an independently collected multilingual benchmark.",
        "retrieval_caveat": "The S track keeps distractor histories; answer markers are present only in gold.jsonl. Requires a separate ingestion/evaluation adapter. Does not test cross-writing-system parity.",
    }
    manifest_bytes = (json.dumps(manifest, ensure_ascii=False, sort_keys=True, indent=2) + "\n").encode()
    (OUT / "manifest.json").write_bytes(manifest_bytes)
    print(json.dumps({
        "source_sha256": actual_source_sha,
        "source_rows_scanned": scanned,
        "selected_count": len(selected),
        "coverage": coverage,
        "input": input_info,
        "gold": gold_info,
        "script_sha256": sha256(Path(__file__).read_bytes()),
        "manifest_sha256": sha256(manifest_bytes),
    }, ensure_ascii=False, sort_keys=True, indent=2))


if __name__ == "__main__":
    main()
