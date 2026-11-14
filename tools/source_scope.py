"""AgentChat verification scope, including one user-approved independent workspace."""

from pathlib import PurePosixPath


def is_agentchat_file(name):
    """Exclude only the separately developed web API/frontend experiment.

    This is a workspace boundary, not a generated-code or experiments exemption.
    Product dependencies on that workspace must fail the crate-graph check.
    """
    return PurePosixPath(name).parts[:2] != ("experiments", "web-api-poc")
