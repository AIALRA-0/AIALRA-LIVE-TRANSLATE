"""Synthetic vectors test privacy-safe uncertainty; real voice scores live outside Git."""

from typing import Any

import numpy as np
import pytest

from workers.model_worker import speakers


class Stream:
    def accept_waveform(self, _rate: int, _audio: Any) -> None:
        pass

    def input_finished(self) -> None:
        pass


class Extractor:
    def __init__(self, mixed: bool = False) -> None:
        self.calls = 0
        self.mixed = mixed

    def create_stream(self) -> Stream:
        return Stream()

    def is_ready(self, _stream: Stream) -> bool:
        return True

    def compute(self, _stream: Stream) -> list[float]:
        vector = [0.0] * 192
        vector[self.calls % 2 if self.mixed else 0] = 1.0
        self.calls += 1
        return vector


def test_disabled_short_silent_and_invalid_audio_never_form_profiles(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.delenv("AIALRA_SPEAKER_MODEL_PATH", raising=False)
    audio = np.full(16000 * 4, 0.02, dtype=np.float32)
    assert speakers.observe(audio, 16000).status == "disabled"
    monkeypatch.setenv("AIALRA_SPEAKER_MODEL_PATH", "synthetic-model")
    assert speakers.observe(audio[:16000], 16000).status == "insufficient"
    assert speakers.observe(audio, 48000).status == "insufficient"
    assert speakers.observe(np.zeros_like(audio), 16000).status == "insufficient"
    assert speakers.observe(np.full_like(audio, np.nan), 16000).status == "uncertain"


def test_consistent_windows_return_one_private_embedding_and_mixed_windows_do_not(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("AIALRA_SPEAKER_MODEL_PATH", "synthetic-model")
    model = Extractor()
    monkeypatch.setattr(speakers, "_overlapping_speech", lambda _audio: False)
    monkeypatch.setattr(speakers, "_extractor", model)
    audio = np.full(16000 * 8, 0.02, dtype=np.float32)
    result = speakers.observe(audio, 16000)
    assert result.status == "observed"
    assert result.provider == "cpu"
    assert len(result.embedding) == 192
    assert result.windows == 3
    assert model.calls == 3
    monkeypatch.setattr(speakers, "_extractor", Extractor(mixed=True))
    result = speakers.observe(audio, 16000)
    assert result.status == "uncertain"
    assert result.embedding == []


def test_extraction_failure_is_a_bounded_observation_not_an_asr_failure(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    def fail(*_args: Any) -> None:
        raise RuntimeError("private path or provider detail")

    monkeypatch.setenv("AIALRA_SPEAKER_MODEL_PATH", "synthetic-model")
    monkeypatch.setattr(speakers, "_overlapping_speech", lambda _audio: False)
    monkeypatch.setattr(speakers, "_extract", fail)
    result = speakers.observe(np.full(64000, 0.02, dtype=np.float32), 16000)
    assert result.status == "unavailable"
    assert "private" not in result.model_dump_json()


def test_overlap_and_missing_detector_never_create_a_single_voice_profile(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("AIALRA_SPEAKER_MODEL_PATH", "synthetic-model")
    audio = np.full(64000, 0.02, dtype=np.float32)
    model = Extractor()
    monkeypatch.setattr(speakers, "_extractor", model)
    monkeypatch.setattr(speakers, "_overlapping_speech", lambda _audio: True)
    assert speakers.observe(audio, 16000).status == "uncertain"
    monkeypatch.setattr(speakers, "_overlapping_speech", lambda _audio: None)
    assert speakers.observe(audio, 16000).status == "unavailable"
    assert model.calls == 0


def test_overlap_detector_pads_its_window_but_ignores_the_padded_frames(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    class Segmentation:
        def run(self, _outputs: None, inputs: dict[str, Any]) -> list[Any]:
            assert inputs["x"].shape == (1, 1, 160000)
            assert not np.any(inputs["x"][0, 0, 64000:])
            scores = np.zeros((1, 589, 7), dtype=np.float32)
            scores[0, :, 1] = 1
            scores[0, 300:, 4] = 2
            return [scores]

    monkeypatch.setattr(speakers, "_segmentation", Segmentation())
    assert speakers._overlapping_speech(np.full(64000, 0.02, dtype=np.float32)) is False


def test_sixteen_second_asr_window_checks_both_ends_and_all_embedding_windows(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("AIALRA_SPEAKER_MODEL_PATH", "synthetic-model")
    model = Extractor()
    monkeypatch.setattr(speakers, "_extractor", model)
    audio = np.linspace(0.01, 0.03, 16000 * 16, dtype=np.float32)
    checked: list[Any] = []

    def no_overlap(clip: Any) -> bool:
        checked.append(clip.copy())
        return False

    monkeypatch.setattr(speakers, "_overlapping_speech", no_overlap)
    result = speakers.observe(audio, 16000)
    assert result.status == "observed"
    assert result.windows == model.calls == 6
    assert len(checked) == 2
    np.testing.assert_array_equal(checked[0], audio[:160000])
    np.testing.assert_array_equal(checked[1], audio[-160000:])
    monkeypatch.setattr(speakers, "_extractor", Extractor(mixed=True))
    assert speakers.observe(audio, 16000).status == "uncertain"
    monkeypatch.setattr(speakers, "_overlapping_speech", lambda clip: bool(clip[-1] > 0.029))
    assert speakers.observe(audio, 16000).status == "uncertain"
    assert speakers.observe(np.ones(16000 * 17, dtype=np.float32), 16000).status == "uncertain"
