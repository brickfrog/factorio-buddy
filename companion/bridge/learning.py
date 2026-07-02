"""Bridge-side evolving memory proposals.

Agents may emit hidden proposal trailers when they discover reusable
procedures or tooling gaps. The bridge persists those trailers as inert local
artifacts. Pending artifacts are not injected back into prompts; only accepted
artifacts are rendered as compact procedural memory.
"""

import os
import sys
from datetime import datetime, timezone
from pathlib import Path

from models import (
    LEARNING_PROPOSAL_KINDS,
    BridgeValidationError,
    LearningProposal,
    LearningProposalCollection,
    LearningProposalDraft,
    LearningRuntimeSettings,
)

LEARNING_TAGS = LEARNING_PROPOSAL_KINDS
MAX_RENDERED_ACCEPTED = 8
MAX_RENDERED_STEPS = 3
MAX_RENDERED_ANTI_STEPS = 2


def _project_root() -> Path:
    return Path(__file__).resolve().parents[2]


def _learning_dir() -> Path:
    return LearningRuntimeSettings.from_env(os.environ).resolved_learning_dir(
        _project_root(),
    )


def _utc_now() -> datetime:
    return datetime.now(timezone.utc)


def _iso_utc(now: datetime | None = None) -> str:
    if now is None:
        now = _utc_now()
    if now.tzinfo is None:
        now = now.replace(tzinfo=timezone.utc)
    return now.astimezone(timezone.utc).isoformat().replace("+00:00", "Z")


def parse_learning_trailer_models(
    source: object,
) -> list[LearningProposal]:
    if not isinstance(source, (str, bytes)):
        proposals = LearningProposalCollection.from_value(source).to_list()
        if proposals:
            return proposals
    return LearningProposal.all_from_trailer_text(source, tags=LEARNING_TAGS)


def _proposal_with_hash(proposal: LearningProposal) -> LearningProposal:
    return proposal.model_copy(update={"content_hash": _candidate_hash(proposal)})


def _candidate_hash(candidate: dict | LearningProposal) -> str:
    proposal = (
        candidate
        if isinstance(candidate, LearningProposal)
        else LearningProposal.coerce(candidate)
    )
    return proposal.stable_content_hash()


def parse_learning_trailers(
    source: object,
) -> list[dict]:
    return [proposal.to_dict() for proposal in parse_learning_trailer_models(source)]


def strip_learning_trailers(text: str) -> str:
    return LearningProposal.strip_trailer_text(text, tags=LEARNING_TAGS)


def _status_dir(status: str) -> Path:
    return _learning_dir() / status


def _proposal_filename(proposal: LearningProposal, now: datetime | None = None) -> str:
    if now is None:
        now = _utc_now()
    stamp = now.astimezone(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    name = LearningProposal.safe_slug(proposal.display_name())
    agent = LearningProposal.safe_slug(proposal.agent, fallback="agent")
    digest = proposal.content_hash or _candidate_hash(proposal)
    return f"{stamp}-{agent}-{name}-{digest}.json"


def save_candidate(
    candidate: dict | LearningProposal,
    status: str = "pending",
    now: datetime | None = None,
    agent_name: str | None = None,
) -> Path | None:
    proposal = LearningProposal.candidate_model(
        candidate,
        agent_name=agent_name,
        status=status,
        default_status="pending",
    )
    if not proposal or not proposal.is_meaningful():
        return None
    proposal = _proposal_with_hash(proposal).model_copy(update={"created_at": _iso_utc(now)})
    path = _status_dir(status) / _proposal_filename(proposal, now)
    tmp = path.with_name(path.name + ".tmp")
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        tmp.write_text(proposal.to_json_text())
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
    proposal = _load_candidate_model(source, default_status="pending")
    if not proposal:
        return None
    proposal = proposal.model_copy(update={
        "status": "accepted",
        "accepted_at": _iso_utc(now),
    })
    target = _status_dir("accepted") / source.name
    tmp = target.with_name(target.name + ".tmp")
    try:
        target.parent.mkdir(parents=True, exist_ok=True)
        tmp.write_text(proposal.to_json_text())
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
    proposal = _load_candidate_model(source, default_status="pending")
    if not proposal:
        return None
    proposal = proposal.model_copy(update={
        "status": "rejected",
        "rejected_at": _iso_utc(now),
    })
    target = _status_dir("rejected") / source.name
    tmp = target.with_name(target.name + ".tmp")
    try:
        target.parent.mkdir(parents=True, exist_ok=True)
        tmp.write_text(proposal.to_json_text())
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


def apply_learning_update(
    agent_name: str,
    source: object,
) -> list[Path]:
    saved = []
    for proposal in parse_learning_trailer_models(source):
        path = save_candidate(proposal, status="pending", agent_name=agent_name)
        if path:
            saved.append(path)
    return saved


def _load_candidate_model(
    path: Path,
    *,
    default_status: str = "accepted",
) -> LearningProposal | None:
    try:
        proposal = LearningProposal.from_file_text(
            path.read_text(),
            default_status=default_status,
        )
    except (BridgeValidationError, OSError):
        return None
    if not proposal or not proposal.is_meaningful():
        return None
    return _proposal_with_hash(proposal)


def _load_candidate_file(path: Path) -> dict | None:
    proposal = _load_candidate_model(path)
    return proposal.to_dict() if proposal else None


def load_accepted_learning_model(
    limit: int = MAX_RENDERED_ACCEPTED,
) -> list[LearningProposal]:
    accepted_dir = _status_dir("accepted")
    try:
        paths = sorted(accepted_dir.glob("*.json"))
    except OSError:
        return []
    candidates: list[LearningProposal] = []
    for path in paths:
        candidate = _load_candidate_model(path, default_status="accepted")
        if candidate:
            candidates.append(candidate)
    candidates.sort(key=lambda item: item.created_at or "")
    try:
        limit = int(limit)
    except (TypeError, ValueError):
        limit = MAX_RENDERED_ACCEPTED
    if limit <= 0:
        return []
    return candidates[-limit:]


def load_accepted_learning(limit: int = MAX_RENDERED_ACCEPTED) -> list[dict]:
    return [candidate.to_dict() for candidate in load_accepted_learning_model(limit)]


def render_accepted_learning(candidates: object) -> str:
    proposals = LearningProposalCollection.from_value(candidates).to_list()
    if not proposals:
        return ""

    lines = ["Accepted learned procedures (reuse when applicable):"]
    for proposal in proposals[-MAX_RENDERED_ACCEPTED:]:
        line = proposal.accepted_memory_line(
            max_steps=MAX_RENDERED_STEPS,
            max_anti_steps=MAX_RENDERED_ANTI_STEPS,
        )
        if line:
            lines.append(line)
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
    candidate = _load_candidate_model(path, default_status="pending")
    if not candidate:
        print(f"{path}: invalid")
        return
    name = candidate.name or candidate.problem or candidate.kind
    print(f"{path}: {candidate.kind} {name}")


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
