"""CPU-only CAM++ observations, never identities or cross-course profiles.

The embedding is private worker-to-Core data, not a transcript field. The Core
owns course-local numbering and stores observations with its existing jobs.
Model: ModelScope iic/speech_campplus_sv_zh_en_16k-common_advanced,
exported by sherpa-onnx. No model is downloaded implicitly during recording.
"""

from __future__ import annotations

import os
import threading
from pathlib import Path
from typing import Any, Literal

import numpy as np
import numpy.typing as npt
from pydantic import BaseModel, Field

MODEL_ID = "campplus-zh-en-16k-v1"
_extractor: Any | None = None
_segmentation: Any | None = None
_lock = threading.Lock()


class SpeakerObservation(BaseModel):
    status: Literal["disabled", "insufficient", "uncertain", "unavailable", "observed"]
    model: str = MODEL_ID
    provider: str = "cpu"
    embedding: list[float] = Field(default_factory=list, max_length=192)
    windows: int = Field(default=0, ge=0, le=8)


def observe(audio: npt.NDArray[np.float32], sample_rate: int) -> SpeakerObservation:
    """Retain uncertainty for short, low-energy or internally inconsistent audio.

    This is not an overlap detector: disagreement means 'unconfirmed', not a
    claim that multiple people were definitely speaking simultaneously.
    """
    model_path = os.getenv("AIALRA_SPEAKER_MODEL_PATH", "")
    if not model_path:
        return SpeakerObservation(status="disabled")
    if sample_rate != 16_000 or audio.ndim != 1 or len(audio) < sample_rate * 2:
        return SpeakerObservation(status="insufficient")
    if len(audio) > sample_rate * 24 or not np.isfinite(audio).all():
        return SpeakerObservation(status="uncertain")
    # Do not derive a profile from room noise or an ASR silence hallucination.
    frames = audio[:len(audio) // 320 * 320].reshape(-1, 320)
    voiced = np.sqrt(np.mean(frames ** 2, axis=1)) >= 0.0032
    if np.count_nonzero(voiced) * 0.02 < 1.0:
        return SpeakerObservation(status="insufficient")
    try:
        with _lock:
            # The detector accepts ten seconds; ASR accepts up to twenty-four.
            # Cover the complete ASR interval with overlapping detector windows,
            # including one window anchored to the tail.
            detector_samples = sample_rate * 10
            detector_step = sample_rate * 8
            starts = list(range(0, max(1, len(audio) - detector_samples + 1), detector_step))
            tail_start = max(0, len(audio) - detector_samples)
            if tail_start not in starts:
                starts.append(tail_start)
            clips = [audio[start:start + detector_samples] for start in starts]
            for clip in clips:
                overlap = _overlapping_speech(clip)
                if overlap is None:
                    return SpeakerObservation(status="unavailable")
                if overlap:
                    return SpeakerObservation(status="uncertain")
            return _extract(audio, sample_rate, model_path)
    except Exception:
        # A diarization failure must not discard a successful ASR result or
        # reveal a local model path in the public course transcript.
        return SpeakerObservation(status="unavailable")


def _overlapping_speech(audio: npt.NDArray[np.float32]) -> bool | None:
    """Use Pyannote's documented powerset cardinalities, not an energy heuristic.

    sherpa-onnx exports segmentation-3.0 with a ten-second window. Zero-padding
    shorter input supplies that window; padded output frames are never counted.
    Classes 0..6 represent {}, {0}, {1}, {2}, {0,1}, {0,2}, {1,2}.
    See pyannote.audio.utils.powerset and sherpa-onnx model metadata.
    """
    global _segmentation
    if _segmentation is None:
        import onnxruntime as ort

        path = os.getenv("AIALRA_SPEAKER_SEGMENTATION_PATH", "")
        if not path or not Path(path).is_file():
            return None
        options = ort.SessionOptions()
        options.intra_op_num_threads = 2
        options.inter_op_num_threads = 1
        options.log_severity_level = 3
        candidate = ort.InferenceSession(
            path, sess_options=options, providers=["CPUExecutionProvider"],
        )
        metadata = candidate.get_modelmeta().custom_metadata_map
        expected = {"model_type": "pyannote-segmentation-3.0", "window_size": "160000",
                    "sample_rate": "16000", "num_classes": "7", "powerset_max_classes": "2",
                    "receptive_field_size": "991", "receptive_field_shift": "270"}
        if any(metadata.get(key) != value for key, value in expected.items()):
            return None
        _segmentation = candidate
    padded = np.pad(audio, (0, 160000 - len(audio)))
    scores = _segmentation.run(None, {"x": padded[None, None, :]})[0][0]
    valid_frames = max(1, 1 + (len(audio) - 991) // 270)
    scores = scores[:valid_frames]
    if scores.ndim != 2 or scores.shape[1] != 7 or not np.isfinite(scores).all():
        return None
    overlap_ms = np.count_nonzero(np.argmax(scores, axis=-1) >= 4) * 270 / 16
    return bool(overlap_ms >= 200)


def _extract(
    audio: npt.NDArray[np.float32], sample_rate: int, model_path: str,
) -> SpeakerObservation:
    global _extractor
    if _extractor is None:
        import sherpa_onnx

        if not Path(model_path).is_file():
            return SpeakerObservation(status="unavailable")
        _extractor = sherpa_onnx.SpeakerEmbeddingExtractor(
            sherpa_onnx.SpeakerEmbeddingExtractorConfig(
                model=model_path, num_threads=2, provider="cpu", debug=False,
            )
        )
    vectors: list[npt.NDArray[np.float32]] = []
    for start in range(0, len(audio), sample_rate * 3):
        clip = audio[start:start + sample_rate * 3]
        if len(clip) < sample_rate * 2:
            # Include the tail in an overlapping full window, never pad silence.
            clip = audio[-min(len(audio), sample_rate * 3):]
        stream = _extractor.create_stream()
        stream.accept_waveform(sample_rate, clip)
        stream.input_finished()
        if not _extractor.is_ready(stream):
            return SpeakerObservation(status="insufficient")
        vector = np.asarray(_extractor.compute(stream), dtype=np.float32)
        norm = float(np.linalg.norm(vector))
        if vector.shape != (192,) or not np.isfinite(vector).all() or norm < 1e-6:
            return SpeakerObservation(status="unavailable")
        vectors.append((vector / norm).astype(np.float32))
    # Reject a single-label assignment when its windows disagree. Averaging two
    # different voices into a new 'speaker' would contaminate future matches.
    if any(float(np.dot(left, right)) < 0.5
           for i, left in enumerate(vectors) for right in vectors[i + 1:]):
        return SpeakerObservation(status="uncertain", windows=len(vectors))
    mean = np.mean(np.stack(vectors), axis=0)
    mean /= np.linalg.norm(mean)
    return SpeakerObservation(
        status="observed", embedding=mean.tolist(), windows=len(vectors),
    )
