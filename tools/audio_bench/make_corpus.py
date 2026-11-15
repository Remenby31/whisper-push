#!/usr/bin/env python3
"""Offline corpus for `whisper-push audio-bench run`.

Same French phrases (macOS `say`, written straight to WAV — nothing is played
aloud) mixed digitally with what a Mac typically plays while you dictate:

  music  — synthesized instrumental (chords, bass, drums), no voice
  song   — the same music + an English voice on top (video, podcast, song)
  noise  — pink noise (fan, air conditioning)

at 10 dB and 0 dB speech-to-interference ratio. For the two signals the MAC
plays (music, song) it also writes a "vp-sim" variant with the interference
attenuated by --erle dB: what an echo canceller removing that much would leave.
That row is a MODEL of voice processing, not a measurement — tools/audio_bench/
live.sh measures the real thing with the microphone.

    python3 tools/audio_bench/make_corpus.py OUT_DIR [--erle 20]
"""
import argparse, os, subprocess, sys, tempfile, wave
import numpy as np

SR = 16_000
PHRASES = [
    "Peux-tu envoyer le compte rendu de la réunion à toute l'équipe avant ce soir",
    "Le déploiement sur le cluster de production est prévu pour jeudi matin",
    "Merci pour ton retour, je regarde la proposition et je reviens vers toi demain",
    "Il faudrait vérifier les logs du serveur avant de relancer le service",
    "Nous avons rendez-vous avec le client à la gare de Lyon vers midi",
    "La nouvelle version corrige le problème de connexion signalé par les utilisateurs",
    "Pense à réserver une salle pour la présentation de la semaine prochaine",
    "Le modèle de transcription tourne entièrement en local sur le Mac",
    "Je préfère qu'on reporte la revue de code à lundi après-midi",
    "Ajoute une option pour désactiver le son au démarrage de l'application",
    "Le budget du projet doit être validé par la direction avant la fin du mois",
    "Est-ce que tu peux me rappeler quand tu as terminé la migration de la base",
]
VOICES = ["Thomas", "Flo (Français (France))", "Jacques", "Sandy (Français (France))"]
LYRICS = ("We were driving down the highway with the radio on, singing every song "
          "we knew until the morning came. Tell me what you want and I will give it "
          "to you, every single day, every single night, all the way back home. ") * 3


def say(text, voice, path):
    subprocess.run(["say", "-v", voice, "-o", path, "--data-format=LEI16@16000", text], check=True)
    with wave.open(path) as w:
        assert w.getframerate() == SR and w.getnchannels() == 1
        return np.frombuffer(w.readframes(w.getnframes()), dtype="<i2").astype(np.float32) / 32768


def write(path, x):
    x = np.clip(x, -1, 1)
    with wave.open(path, "wb") as w:
        w.setnchannels(1); w.setsampwidth(2); w.setframerate(SR)
        w.writeframes((x * 32767).astype("<i2").tobytes())


def music(secs, rng):
    """Four-chord loop at 112 bpm: saw-ish pads, bass, kick, snare, hats."""
    n = int(secs * SR); t = np.arange(n) / SR
    beat = 60 / 112; bar = 4 * beat
    chords = [[220.0, 277.18, 329.63], [196.0, 246.94, 293.66], [174.61, 220.0, 261.63], [196.0, 246.94, 329.63]]
    out = np.zeros(n, np.float32)
    idx = (t // bar).astype(int) % 4
    for k, ch in enumerate(chords):
        m = idx == k
        for f in ch:
            for h in range(1, 6):
                out[m] += 0.05 / h * np.sin(2 * np.pi * f * h * t[m])
        out[m] += 0.25 * np.sin(2 * np.pi * ch[0] / 2 * t[m])  # bass
    pos = t % beat; env = np.exp(-pos * 30)
    out += 0.5 * np.sin(2 * np.pi * (50 + 60 * np.exp(-pos * 40)) * pos) * env  # kick
    half = (t % (2 * beat)) - beat; snare = (half >= 0) * np.exp(-np.abs(half) * 25)
    out += 0.25 * rng.standard_normal(n) * snare
    hat = np.exp(-(t % (beat / 2)) * 120)
    out += 0.06 * np.diff(rng.standard_normal(n + 1)) * hat
    return out.astype(np.float32)


def pink(n, rng):
    f = np.fft.rfftfreq(n, 1 / SR); f[0] = 1
    spec = (rng.standard_normal(len(f)) + 1j * rng.standard_normal(len(f))) / np.sqrt(f)
    x = np.fft.irfft(spec, n)
    return (x / np.abs(x).max()).astype(np.float32)


def rms(x):
    return float(np.sqrt(np.mean(x.astype(np.float64) ** 2)) + 1e-12)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("out")
    ap.add_argument("--erle", type=float, default=20.0, help="dB removed in the vp-sim rows")
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)
    rng = np.random.default_rng(7)
    tmp = tempfile.mkdtemp()

    lyrics = say(LYRICS, "Samantha", os.path.join(tmp, "lyrics.wav"))
    long_music = music(len(lyrics) / SR + 1, rng)[: len(lyrics)]
    interferers = {
        "music": long_music,
        "song": long_music / rms(long_music) * 0.5 + lyrics / rms(lyrics) * 0.5,
        "noise": pink(len(lyrics), rng),
    }
    # Full-length interferers, for live.sh to play through the speakers.
    for kind in ("music", "song"):
        x = interferers[kind]
        write(os.path.join(a.out, f"interferer_{kind}.wav"), x / np.abs(x).max() * 0.7)
    rows = []
    for i, text in enumerate(PHRASES):
        speech = say(text, VOICES[i % len(VOICES)], os.path.join(tmp, f"s{i}.wav"))
        speech = np.concatenate([np.zeros(SR // 2, np.float32), speech, np.zeros(SR // 2, np.float32)])
        speech *= 0.1 / rms(speech)  # ≈ −20 dBFS, a normal dictation level
        name = f"p{i:02d}"
        write(os.path.join(a.out, f"{name}_clean.wav"), speech)
        rows.append((f"{name}_clean.wav", "clean", text))
        for kind, src in interferers.items():
            off = rng.integers(0, len(src) - len(speech))
            seg = src[off: off + len(speech)]
            seg = seg / rms(seg) * rms(speech)  # 0 dB reference
            for snr in (10, 0):
                for sim in ([False, True] if kind != "noise" else [False]):
                    att = 10 ** (-(snr + (a.erle if sim else 0)) / 20)
                    cond = f"{kind}_{snr}dB" + (f"_vp-sim{a.erle:g}" if sim else "")
                    f = f"{name}_{cond}.wav"
                    write(os.path.join(a.out, f), speech + seg * att)
                    rows.append((f, cond, text))
    with open(os.path.join(a.out, "manifest.tsv"), "w") as m:
        m.write("# path\tcondition\treference\n")
        for r in rows:
            m.write("\t".join(r) + "\n")
    print(f"{len(rows)} clips → {a.out}/manifest.tsv")


if __name__ == "__main__":
    sys.exit(main())
