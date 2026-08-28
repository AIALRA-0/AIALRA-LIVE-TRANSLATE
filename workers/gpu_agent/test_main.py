"""Unit tests cover identifiers and secret-free CUDA metadata shaping."""

from workers.gpu_agent.main import sanitize_worker_id


def test_worker_identifier_removes_network_and_path_punctuation() -> None:
    assert sanitize_worker_id("rtx host:C:\\private/path") == "rtxhostCprivatepath"


def test_worker_identifier_is_bounded() -> None:
    assert len(sanitize_worker_id("x" * 100)) == 64
