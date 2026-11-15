#!/usr/bin/env bash
# Live A/B of macOS voice processing — the part only a human with a mic can do.
#
#   1. Mac speakers on (no headphones), volume ~50 %, quiet room.
#   2. In Terminal, from the repo:  ./tools/audio_bench/live.sh
#   3. Read each phrase aloud when told (twice: once raw, once processed).
#
# While you speak, the Mac plays a song (music + an English voice) through its
# speakers. Each phrase is recorded raw, then through VoiceProcessingIO, with
# the app's own capture code (`whisper-push audio-bench record`). Then two
# "song only, stay silent" takes measure how many dB the echo canceller
# removes, and every take is transcribed → WER per mode. Results land in
# ~/whisper-push-vp-bench/<timestamp>/ (report.md). ~3 minutes.
set -euo pipefail
cd "$(dirname "$0")/../.."
OUT="$HOME/whisper-push-vp-bench/$(date +%Y%m%d-%H%M%S)"
mkdir -p "$OUT"
BIN=./target/debug/whisper-push
[ -x "$BIN" ] || cargo build
[ -f "$OUT/../corpus/interferer_song.wav" ] || python3 -I tools/audio_bench/make_corpus.py "$OUT/../corpus" >/dev/null
SONG="$OUT/../corpus/interferer_song.wav"
SECS=7

PHRASES=(
  "Peux-tu envoyer le compte rendu de la réunion à toute l'équipe avant ce soir"
  "Le déploiement sur le cluster de production est prévu pour jeudi matin"
  "Il faudrait vérifier les logs du serveur avant de relancer le service"
  "Nous avons rendez-vous avec le client à la gare de Lyon vers midi"
  "Pense à réserver une salle pour la présentation de la semaine prochaine"
  "Le budget du projet doit être validé par la direction avant la fin du mois"
)

play() { afplay -v 0.6 "$SONG" & PLAYER=$!; sleep 0.5; }
stop_play() { kill "$PLAYER" 2>/dev/null || true; wait "$PLAYER" 2>/dev/null || true; }
trap stop_play EXIT
take() { # $1=out.wav $2=raw|vp
  local flag=(); [ "$2" = vp ] && flag=(--vp)
  play
  "$BIN" audio-bench record "$1" ${flag[@]+"${flag[@]}"} --secs "$SECS" 2>/dev/null | tee -a "$OUT/takes.log"
  stop_play
}

echo "# path	condition	reference" > "$OUT/manifest.tsv"
i=0
for p in "${PHRASES[@]}"; do
  # Alternate which mode goes first so fatigue/learning doesn't favour one.
  if (( i % 2 )); then modes=(vp raw); else modes=(raw vp); fi
  for m in "${modes[@]}"; do
    printf '\n[%s] Lisez dès le bip (%ss) :\n  « %s »\n' "$m" "$SECS" "$p"
    sleep 1.5; printf '\a'
    take "$OUT/p$i-$m.wav" "$m"
    printf 'p%s-%s.wav\tlive_%s\t%s\n' "$i" "$m" "$m" "$p" >> "$OUT/manifest.tsv"
  done
  i=$((i + 1))
done

printf '\nDernière étape : restez SILENCIEUX pendant deux prises de %ss.\n' "$SECS"
sleep 2
take "$OUT/silent-raw.wav" raw
take "$OUT/silent-vp.wav" vp

{
  echo "# Voice processing — live A/B ($(date '+%Y-%m-%d %H:%M'))"
  echo
  echo '```'; cat "$OUT/takes.log"; echo '```'
  python3 -I - "$OUT" <<'PY'
import sys, wave, array, math
out = sys.argv[1]
def rms(p):
    with wave.open(p) as w:
        a = [x / 32768 for x in array.array("h", w.readframes(w.getnframes()))]
    return math.sqrt(sum(x * x for x in a) / max(len(a), 1)) + 1e-12
r, v = rms(f"{out}/silent-raw.wav"), rms(f"{out}/silent-vp.wav")
print(f"\nSong alone, nobody speaking: raw {20*math.log10(r):.1f} dBFS, "
      f"voice processing {20*math.log10(v):.1f} dBFS → {20*math.log10(r/v):.1f} dB removed\n")
PY
  "$BIN" audio-bench run "$OUT/manifest.tsv" 2>/dev/null | sed -n '/^## /,$p'
} > "$OUT/report.md"
cat "$OUT/report.md"
echo; echo "→ $OUT/report.md"
