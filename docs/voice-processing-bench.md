# Voice processing: bench (2026-11-15)

Question: when the Mac plays audio while you dictate, does macOS voice
processing (VoiceProcessingIO: echo cancellation, noise suppression, AGC) make
transcription better, and what does it cost?

## Offline (reproducible, no microphone)

```bash
python3 -I tools/audio_bench/make_corpus.py /tmp/vp-corpus      # 132 clips, ~5 s
./target/debug/whisper-push audio-bench run /tmp/vp-corpus/manifest.tsv
./target/debug/whisper-push audio-bench run /tmp/vp-corpus/manifest.tsv --model ggml-large-v3-turbo-q5_0.bin
```

There are 12 French phrases (macOS `say`, 4 voices, written straight to WAV, nothing
played aloud), mixed digitally with:

- **music**: a synthesized instrumental;
- **song**: the same music plus an English voice, i.e. what a video, podcast or song puts
  in the room;
- **noise**: pink noise, i.e. a fan.

Each mix is made at 10 dB and 0 dB speech-to-interference ratio. The `vp-sim20` rows are
the same mix with the Mac's audio reduced by 20 dB, which is what an echo canceller
removing 20 dB would leave. **They are a model of voice processing, not a measurement.**
The live script below measures the real thing.

The pipeline is the production one: Silero VAD compaction → engine, no dictionary.
Language is `fr`. Apple M-series, debug build.

| condition | Parakeet ultra int8 WER | Whisper large-v3-turbo WER |
|---|---:|---:|
| clean | 2.5 % | 1.9 % |
| music 10 dB | 2.5 % | 0.6 % |
| music 0 dB | 3.8 % | 8.2 % |
| music 0 dB, vp-sim | 1.9 % | 0.6 % |
| **song 10 dB** | 5.0 % | 10.1 % |
| song 10 dB, vp-sim | 3.1 % | 3.1 % |
| **song 0 dB** | **47.2 %** | **51.6 %** |
| **song 0 dB, vp-sim** | **1.9 %** | **3.1 %** |
| noise 10 dB | 9.4 % | 16.4 % |
| noise 0 dB | 35.8 % | 49.1 % |

Latency per clip, 5 s of audio: Parakeet ≈ 190 ms and Whisper ≈ 520 ms, with no measurable
difference between conditions. The engines don't slow down on noisy audio.

What this says:

- **Instrumental music barely matters.** Both engines ignore it, even at equal loudness.
- **A voice in what the Mac plays is the real problem.** At equal loudness, half the words
  are wrong: the engines transcribe the other voice. Removing 20 dB of it brings the WER
  back to the clean level. This is exactly the case echo cancellation targets, because the
  Mac knows what it is playing.
- **Steady noise also hurts a lot.** Echo cancellation can't help with noise, since it
  doesn't come from the Mac. VPIO's noise suppressor might, which only the live run can
  tell.
- **Unmeasured costs:** VPIO's own artifacts on the voice (suppression, AGC pumping) and
  its open time. The live run measures both.

## Live (Baptiste, ~3 min)

```bash
./tools/audio_bench/live.sh     # from Terminal, speakers on, no headphones, volume ~50 %
```

The script plays the "song" through the speakers while you read 6 phrases, each once raw
and once through VoiceProcessingIO (the order alternates). It then records two silent
takes to measure how many dB are removed, and reports the WER per mode plus the time to
open the stream. Output goes to `~/whisper-push-vp-bench/<timestamp>/report.md`.

This is also the first run of the VPIO code on real hardware. The agent that wrote it
could not open a microphone without triggering a permission prompt.
