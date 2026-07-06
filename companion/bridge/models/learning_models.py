"""Compatibility re-exports for learning-owned bridge models."""

from __future__ import annotations

from learning import (
    LEARNING_PROPOSAL_KINDS,
    LEARNING_PROPOSAL_STATUSES,
    MAX_LEARNING_PROPOSAL_FIELD_ITEMS,
    LearningProposal,
    LearningProposalCollection,
    LearningProposalDraft,
    LearningProposalDraftBodyBuilder,
    _optional_learning_timestamp,
    _proposal_kind,
    _proposal_status,
)

__all__ = [
    "LEARNING_PROPOSAL_KINDS",
    "LEARNING_PROPOSAL_STATUSES",
    "MAX_LEARNING_PROPOSAL_FIELD_ITEMS",
    "LearningProposal",
    "LearningProposalCollection",
    "LearningProposalDraft",
    "LearningProposalDraftBodyBuilder",
    "_optional_learning_timestamp",
    "_proposal_kind",
    "_proposal_status",
]
