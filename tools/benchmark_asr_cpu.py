"""Benchmark faster-whisper CPU thread counts without printing transcript content."""

from __future__ import annotations

import argparse
import json
import statistics
import time
import wave
from pathlib import Path

import numpy as np
from faster_whisper import WhisperModel


def percentile(values: list[float], quantile: float) -> float:
    """Use the nearest-rank percentile used by the deployment validation report."""

    ordered = sorted(values)
    return ordered[max(0, min(len(ordered) - 1, int(len(ordered) * quantile + 0.999999) - 1))]


def read_window(path: Path, seconds: int) -> tuple[np.ndarray, int]:
    """Read one mono PCM16 window from a private WAV fixture."""

    with wave.open(str(path), "rb") as source:
        if source.getsampwidth() != 2 or source.getnchannels() != 1:
            raise ValueError("fixture must be mono PCM16 WAV")
        sample_rate = source.getframerate()
        raw = source.readframes(sample_rate * seconds)
    return np.frombuffer(raw, dtype="<i2").astype(np.float32) / 32768.0, sample_rate


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("fixture", type=Path)
    parser.add_argument("--model", default="base")
    parser.add_argument("--threads", type=int, required=True)
    parser.add_argument("--iterations", type=int, default=12)
    parser.add_argument("--seconds", type=int, default=8)
    args = parser.parse_args()
    if not 1 <= args.threads <= 32:
        raise SystemExit("threads must be between 1 and 32")

    audio, _sample_rate = read_window(args.fixture, args.seconds)
    model = WhisperModel(args.model, device="cpu", compute_type="int8", cpu_threads=args.threads)
    durations: list[float] = []
    nonempty = 0
    for _ in range(args.iterations + 1):
        started = time.perf_counter()
        segments, _info = model.transcribe(
            audio,
            language="en",
            beam_size=1,
            best_of=1,
            vad_filter=True,
            condition_on_previous_text=False,
        )
        has_text = any(segment.text.strip() for segment in segments)
        elapsed_ms = (time.perf_counter() - started) * 1000
        if durations:
            nonempty += int(has_text)
        durations.append(elapsed_ms)

    measured = durations[1:]
    print(
        json.dumps(
            {
                "model": args.model,
                "threads": args.threads,
                "iterations": len(measured),
                "nonempty": nonempty,
                "p50_ms": round(statistics.median(measured), 2),
                "p95_ms": round(percentile(measured, 0.95), 2),
                "max_ms": round(max(measured), 2),
            },
            separators=(",", ":"),
        )
    )


if __name__ == "__main__":
    main()
