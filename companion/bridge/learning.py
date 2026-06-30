"""Bridge-side evolving memory proposals.

Agents may emit hidden proposal trailers when they discover reusable
procedures or tooling gaps. The bridge persists those trailers as inert local
artifacts. Pending artifacts are not injected back into prompts; only accepted
artifacts are rendered as compact procedural memory.
"""

import hashlib
import json
import os
import re
import sys
from datetime import datetime, timezone
from pathlib import Path

from models import LEARNING_PROPOSAL_KINDS, LearningProposal

LEARNING_TAGS = LEARNING_PROPOSAL_KINDS
LEARNING_RE = re.compile(
    r"<(?P<tag>skill_proposal|diagnostic_proposal|script_proposal|bug_report)>"
    r"(?P<body>.*?)"
    r"</(?P=tag)>",
    re.DOTALL | re.IGNORECASE,
)
MAX_RENDERED_ACCEPTED = 8
MAX_RENDERED_STEPS = 3
MAX_RENDERED_ANTI_STEPS = 2
MAX_FIELD_ITEMS = 20


def _project_root() -> Path:
    return Path(__file__).resolve().parents[2]


def _learning_dir() -> Path:
    configured = os.environ.get("BRIDGE_LEARNING_DIR", "").strip()
    if configured:
        return Path(configured)
    return _project_root() / ".factorioctl" / "learned"


def _utc_now() -> datetime:
    return datetime.now(timezone.utc)


def _iso_utc(now: datetime | None = None) -> str:
    if now is None:
        now = _utc_now()
    if now.tzinfo is None:
        now = now.replace(tzinfo=timezone.utc)
    return now.astimezone(timezone.utc).isoformat().replace("+00:00", "Z")


def _safe_name(value: str, fallback: str = "proposal") -> str:
    slug = re.sub(r"[^a-zA-Z0-9_-]+", "-", str(value).strip().lower())
    slug = re.sub(r"-{2,}", "-", slug).strip("-")
    return slug[:80] or fallback


def _str_list(value) -> list[str]:
    if isinstance(value, list):
        return [str(item).strip() for item in value if str(item).strip()][:MAX_FIELD_ITEMS]
    if isinstance(value, str) and value.strip():
        return [value.strip()]
    return []


def _normalize_kind(tag: str) -> str:
    tag = str(tag).strip().lower()
    if tag in LEARNING_TAGS:
        return tag
    return "skill_proposal"


def _parse_list_value(value: str) -> list[str]:
    if not isinstance(value, str):
        return []
    raw = value.strip()
    if not raw:
        return []
    if "," not in raw:
        return [raw]
    return [part.strip() for part in raw.split(",") if part.strip()]


def _parse_proposal_body(kind: str, body: str) -> dict:
    parsed = {
        "kind": _normalize_kind(kind),
        "name": "",
        "trigger": "",
        "problem": "",
        "preconditions": [],
        "steps": [],
        "anti_steps": [],
        "evidence": [],
        "acceptance_tests": [],
        "raw_body": str(body),
    }
    active_key = None
    list_keys = {
        "preconditions",
        "steps",
        "anti_steps",
        "evidence",
        "acceptance_tests",
    }
    aliases = {
        "title": "name",
        "summary": "problem",
        "avoid": "anti_steps",
        "anti-step": "anti_steps",
        "anti-steps": "anti_steps",
        "anti_steps": "anti_steps",
        "acceptance": "acceptance_tests",
        "acceptance_test": "acceptance_tests",
        "acceptance_tests": "acceptance_tests",
        "test": "acceptance_tests",
        "tests": "acceptance_tests",
    }

    for raw_line in str(body).splitlines():
        line = raw_line.strip()
        if not line:
            continue
        key, sep, value = line.partition(":")
        key_lower = key.strip().lower().replace(" ", "_")
        key_lower = aliases.get(key_lower, key_lower)
        if sep and key_lower in parsed:
            if key_lower in list_keys:
                active_key = key_lower
                parsed[key_lower].extend(_parse_list_value(value))
            else:
                parsed[key_lower] = value.strip()
                active_key = None
            continue
        if active_key and line.startswith("- "):
            item = line[2:].strip()
            if item:
                parsed[active_key].append(item)

    for key in list_keys:
        parsed[key] = _str_list(parsed.get(key, []))
    return LearningProposal.coerce(parsed).to_dict()


def _is_meaningful(candidate: dict) -> bool:
    if not isinstance(candidate, dict):
        return False
    status = candidate.get("status", "pending")
    return LearningProposal.coerce(candidate, status=status).is_meaningful()


def _normalize_candidate(candidate: dict, agent_name: str, status: str = "pending") -> dict | None:
    if not isinstance(candidate, dict):
        return None
    proposal = LearningProposal.coerce(candidate, agent_name=agent_name, status=status)
    if not proposal.is_meaningful():
        return None
    normalized = proposal.to_dict()
    normalized["content_hash"] = _candidate_hash(normalized)
    return normalized


def _hash_payload(candidate: dict | LearningProposal) -> dict:
    if isinstance(candidate, LearningProposal):
        return candidate.hash_payload()
    return LearningProposal.coerce(candidate).hash_payload()


def _candidate_hash(candidate: dict) -> str:
    payload = json.dumps(_hash_payload(candidate), sort_keys=True, separators=(",", ":"))
    return hashlib.sha256(payload.encode("utf-8")).hexdigest()[:16]


def parse_learning_trailers(text: str) -> list[dict]:
    if not isinstance(text, str):
        return []
    proposals = []
    for match in LEARNING_RE.finditer(text):
        parsed = _parse_proposal_body(match.group("tag"), match.group("body"))
        if _is_meaningful(parsed):
            proposals.append(parsed)
    return proposals


def strip_learning_trailers(text: str) -> str:
    if not isinstance(text, str):
        return ""
    if not LEARNING_RE.search(text):
        return text
    stripped = LEARNING_RE.sub("", text)
    stripped = re.sub(r"\n{3,}", "\n\n", stripped)
    return stripped.strip()


def _status_dir(status: str) -> Path:
    return _learning_dir() / status


def _candidate_filename(candidate: dict, now: datetime | None = None) -> str:
    if now is None:
        now = _utc_now()
    stamp = now.astimezone(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    name = _safe_name(candidate.get("name") or candidate.get("problem") or candidate.get("kind"))
    agent = _safe_name(candidate.get("agent", "agent"), "agent")
    digest = candidate.get("content_hash") or _candidate_hash(candidate)
    return f"{stamp}-{agent}-{name}-{digest}.json"


def save_candidate(candidate: dict, status: str = "pending", now: datetime | None = None) -> Path | None:
    agent_name = candidate.get("agent", "unknown") if isinstance(candidate, dict) else "unknown"
    normalized = _normalize_candidate(candidate, agent_name, status=status)
    if not normalized:
        return None
    normalized["created_at"] = _iso_utc(now)
    path = _status_dir(status) / _candidate_filename(normalized, now)
    tmp = path.with_name(path.name + ".tmp")
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        tmp.write_text(json.dumps(normalized, indent=2, sort_keys=True) + "\n")
        os.replace(tmp, path)
    except OSError as e:
        print(f"[learning] WARNING: failed to persist learning candidate: {e}")
        try:
            tmp.unlink(missing_ok=True)
        except OSError:
            pass
        return None
    return path


def promote_candidate(path: str | Path, now: datetime | None = None) -> Path | None:
    source = Path(path)
    candidate = _load_candidate_file(source)
    if not candidate:
        return None
    candidate["status"] = "accepted"
    candidate["accepted_at"] = _iso_utc(now)
    target = _status_dir("accepted") / source.name
    tmp = target.with_name(target.name + ".tmp")
    try:
        target.parent.mkdir(parents=True, exist_ok=True)
        tmp.write_text(json.dumps(candidate, indent=2, sort_keys=True) + "\n")
        os.replace(tmp, target)
        try:
            source.unlink()
        except OSError:
            pass
    except OSError as e:
        print(f"[learning] WARNING: failed to promote learning candidate: {e}")
        try:
            tmp.unlink(missing_ok=True)
        except OSError:
            pass
        return None
    return target


def reject_candidate(path: str | Path, now: datetime | None = None) -> Path | None:
    source = Path(path)
    candidate = _load_candidate_file(source)
    if not candidate:
        return None
    candidate["status"] = "rejected"
    candidate["rejected_at"] = _iso_utc(now)
    target = _status_dir("rejected") / source.name
    tmp = target.with_name(target.name + ".tmp")
    try:
        target.parent.mkdir(parents=True, exist_ok=True)
        tmp.write_text(json.dumps(candidate, indent=2, sort_keys=True) + "\n")
        os.replace(tmp, target)
        try:
            source.unlink()
        except OSError:
            pass
    except OSError as e:
        print(f"[learning] WARNING: failed to reject learning candidate: {e}")
        try:
            tmp.unlink(missing_ok=True)
        except OSError:
            pass
        return None
    return target


def pending_candidates() -> list[Path]:
    try:
        return sorted(_status_dir("pending").glob("*.json"))
    except OSError:
        return []


def apply_learning_update(agent_name: str, text: str) -> list[Path]:
    saved = []
    for proposal in parse_learning_trailers(text):
        proposal["agent"] = agent_name
        path = save_candidate(proposal, status="pending")
        if path:
            saved.append(path)
    return saved


def _load_candidate_file(path: Path) -> dict | None:
    try:
        data = json.loads(path.read_text())
    except (ValueError, OSError):
        return None
    if not isinstance(data, dict):
        return None
    normalized = _normalize_candidate(data, data.get("agent", "unknown"), status=data.get("status", "accepted"))
    if not normalized:
        return None
    created_at = data.get("created_at")
    if isinstance(created_at, str):
        normalized["created_at"] = created_at
    return normalized


def load_accepted_learning(limit: int = MAX_RENDERED_ACCEPTED) -> list[dict]:
    accepted_dir = _status_dir("accepted")
    try:
        paths = sorted(accepted_dir.glob("*.json"))
    except OSError:
        return []
    candidates = []
    for path in paths:
        candidate = _load_candidate_file(path)
        if candidate:
            candidates.append(candidate)
    candidates.sort(key=lambda item: str(item.get("created_at", "")))
    try:
        limit = int(limit)
    except (TypeError, ValueError):
        limit = MAX_RENDERED_ACCEPTED
    if limit <= 0:
        return []
    return candidates[-limit:]


def _short_items(items: list[str], limit: int) -> str:
    values = _str_list(items)[:limit]
    return "; ".join(values)


def render_accepted_learning(candidates: list[dict]) -> str:
    if not isinstance(candidates, list):
        return ""
    normalized = [
        _normalize_candidate(candidate, candidate.get("agent", "unknown"), status="accepted")
        for candidate in candidates
        if isinstance(candidate, dict)
    ]
    normalized = [candidate for candidate in normalized if candidate]
    if not normalized:
        return ""

    lines = ["Accepted learned procedures (reuse when applicable):"]
    for candidate in normalized[-MAX_RENDERED_ACCEPTED:]:
        name = candidate.get("name") or candidate.get("problem") or candidate.get("kind")
        parts = []
        trigger = candidate.get("trigger") or candidate.get("problem")
        if trigger:
            parts.append(f"when {trigger}")
        steps = _short_items(candidate.get("steps", []), MAX_RENDERED_STEPS)
        if steps:
            parts.append(f"do {steps}")
        anti_steps = _short_items(candidate.get("anti_steps", []), MAX_RENDERED_ANTI_STEPS)
        if anti_steps:
            parts.append(f"avoid {anti_steps}")
        if not parts:
            evidence = _short_items(candidate.get("evidence", []), 1)
            if evidence:
                parts.append(evidence)
        if parts:
            lines.append(f"- {name}: " + "; ".join(parts))
    return "\n".join(lines) if len(lines) > 1 else ""


def learning_proposal_prompt() -> str:
    return (
        "If this run reveals a reusable procedure, repeated failure mode, or "
        "tooling gap that would help future runs, emit at most one hidden "
        "<skill_proposal>, <diagnostic_proposal>, <script_proposal>, or "
        "<bug_report> block. Use fields like name, trigger/problem, "
        "preconditions, steps, anti_steps, evidence, and acceptance_tests. "
        "These proposals are inert local artifacts; do not include secrets, "
        "raw credentials, or requests for unrestricted repo access."
    )


def _print_candidate(path: Path) -> None:
    candidate = _load_candidate_file(path)
    if not candidate:
        print(f"{path}: invalid")
        return
    name = candidate.get("name") or candidate.get("problem") or candidate.get("kind")
    print(f"{path}: {candidate.get('kind')} {name}")


def main(argv: list[str] | None = None) -> int:
    if argv is None:
        argv = sys.argv[1:]
    command = argv[0] if argv else "list"
    if command == "list":
        for path in pending_candidates():
            _print_candidate(path)
        return 0
    if command in {"accept", "promote"} and len(argv) == 2:
        target = promote_candidate(argv[1])
        if not target:
            print("failed to promote candidate", file=sys.stderr)
            return 1
        print(target)
        return 0
    if command == "reject" and len(argv) == 2:
        target = reject_candidate(argv[1])
        if not target:
            print("failed to reject candidate", file=sys.stderr)
            return 1
        print(target)
        return 0
    print(
        "usage: learning.py [list] | accept <pending.json> | reject <pending.json>",
        file=sys.stderr,
    )
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
