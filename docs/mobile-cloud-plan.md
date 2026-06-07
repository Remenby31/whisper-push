# Spec — Whisper Push Mobile (Expo + Cloud API), v1

> Porter Whisper Push sur téléphone. Le **desktop reste 100 % local** (l'app Rust
> actuelle ne change pas). Le **mobile est un produit séparé** : client **Expo /
> React Native** (iOS + Android), transcription **100 % cloud API** réutilisant le
> cœur Rust. **iPhone prioritaire** si arbitrage.
>
> Recherche source : 2 workflows multi-agents, faits vérifiés de façon adversariale
> (cf. §16 Sources). Plusieurs « évidences » du web ont été corrigées — signalées ⚠️.

---

## 0. Invariants (non négociables)

- **I1 — Le desktop ne bouge pas.** Le binaire Rust actuel (`whisper-push`) reste
  100 % local, hold-to-talk, clipboard+paste. Le mobile est un *deuxième front-end*
  qui parle à un *service*, pas une refonte.
- **I2 — Le serveur réutilise une LIB, pas la coquille desktop.** On extrait un crate
  portable `whisper-push-core` (decode + transcribe) ; les deps desktop (`cpal`,
  `tray-icon`, `enigo`, `winit`, `objc2`…) **ne compilent pas** côté serveur.
- **I3 — Le clavier/IME natif est minimal.** Toute la logique lourde (capture micro,
  appel API, état) vit dans l'app conteneur (iOS) ou le service IME (Android) — jamais
  dans la surface clavier (cap mémoire iOS ~48 Mo, et RN ne tourne pas dans une
  extension/IME, cf. §1).
- **I4 — Privacy = différenciateur.** TLS en transit ; **zéro rétention audio par
  défaut** (l'inverse du défaut de Wispr — cf. §15). Auth par user.
- **I5 — Dégradation honnête.** Pas de réseau → message clair. Pas de transcription
  offline en v1 mobile (assumé : c'est le prix du « modèle dans le cloud »).

---

## 1. Les 3 contraintes plateforme qui dictent TOUT (vérifiées)

| | **iOS** | **Android** |
|---|---|---|
| Micro depuis le clavier | **❌ Impossible** (doc Apple : *« custom keyboards… have no access to the device microphone »*) — même avec Full Access | **✅ OK** dans l'IME (`RECORD_AUDIO`) |
| Réseau depuis le clavier | Full Access requis (`RequestsOpenAccess`) | Permission `INTERNET` normale |
| Modèle local dans le clavier | ❌ cap ~48 Mo (jetsam) | ✅ possible (FUTO/Transcribro tournent whisper.cpp dans l'IME) |
| RN/JS dans le clavier | ❌ (crash mémoire, `facebook/react-native#31910`) | ❌ (IME = `Service` sans `Activity`, `getCurrentActivity()==null`, `#7762`) |
| Insertion texte | `insertText:` sur `UITextDocumentProxy` | `InputConnection.commitText()` |
| Conséquence | **app conteneur (micro) + clavier (insertion) + App Group** | **IME natif autonome** (enregistre + POST + insère) |

**Les 3 invariants techniques à graver :**

1. **iOS : un clavier n'a pas le micro.** Donc tout « clavier vocal » iOS *doit* être
   `app conteneur + extension clavier`, reliés par un **App Group**. C'est exactement
   le pattern « Flow Session » de Wispr. ⚠️ iOS 26.4 a en plus cassé le retour auto →
   l'utilisateur doit *swiper* pour revenir.
2. **Le clavier (iOS) / l'IME (Android) est forcément natif** (Swift / Kotlin). Expo
   ne peut PAS écrire un clavier système en JS. Expo sert à l'**app shell** + logique
   partagée ; les extensions sont des **cibles natives** générées par config plugin.
3. **Android est bien plus simple qu'iOS** : l'IME accède micro + réseau directement et
   « current input method » est une exemption explicite au blocage de foreground-service
   d'Android 12+. (Gboard enregistre comme ça.)

---

## 2. Architecture cible (vue d'ensemble)

```
┌────────────────────────────────────────────────────────────────────┐
│  EXPO APP (iOS + Android) — React Native / TypeScript               │
│  onboarding · auth · réglages · UI d'enregistrement · historique    │
│  expo-audio (capture .m4a) · client API (Bearer) · expo-clipboard   │
└───────────────┬───────────────────────────────┬────────────────────┘
                │ (App Group / Darwin notif)     │ (bind IME)
        ┌───────▼─────────┐              ┌────────▼──────────┐
        │ iOS Keyboard Ext │              │ Android IME       │
        │ (Swift natif)    │              │ (Kotlin natif)    │
        │ insertText:      │              │ commitText()      │
        └───────┬──────────┘              └────────┬──────────┘
                │ HTTPS multipart (audio .m4a)      │
                ▼                                   ▼
        ┌──────────────────────────────────────────────────┐
        │  CLOUD : axum + crate `whisper-push-core`          │
        │  upload → load_audio_file (symphonia) → transcribe │
        │  auth (API key / JWT) + rate-limit (tower-governor)│
        │  GPU L4/T4 scale-to-zero (Modal / RunPod), warm    │
        └──────────────────────────────────────────────────┘
```

> ⚠️ **Correction d'un verifier** : il a « réfuté » le serveur axum — mais en croyant
> qu'il tournerait *sur le téléphone* (iOS suspend les apps en arrière-plan). Faux ici :
> **le serveur est dans le cloud**, le téléphone ne fait qu'**uploader** un clip. Son
> point valide est repris dans I2 : extraire une lib portable.

---

## 3. Le serveur cloud (réutilisation maximale du cœur Rust)

### 3.1 Refactor préalable — extraire `crates/whisper-push-core`
Le workspace a **déjà** ce pattern (`crates/whisper-push-dict`). On ajoute un crate
portable qui contient **uniquement** la chaîne de transcription :

```
crates/whisper-push-core/        # PORTABLE (pas de cpal/tray/enigo/objc2)
  src/
    decode.rs        # ← déplacé de src/audio/decode.rs (symphonia + rubato, intact)
    transcribe.rs    # ← extrait de src/transcribe/mod.rs : load_model / unload /
                     #   transcribe_with_backend (whisper-rs ; parakeet optionnel)
  Cargo.toml         # deps : whisper-rs, symphonia, rubato, anyhow, tracing
                     #        + features cuda/metal/vulkan, parakeet (opt)
crates/whisper-push-server/      # NOUVEAU binaire serveur
  src/main.rs        # axum + auth + rate-limit + handler /transcribe
src/                             # binaire desktop (inchangé) → dépend de -core par path
```

Le desktop **et** le serveur dépendent de `whisper-push-core`. Une seule logique de
transcription, un seul modèle (`ggml-large-v3-turbo-q5_0.bin`).

### 3.2 Le handler (≈ 80–150 lignes)
```
POST /v1/transcribe   (Authorization: Bearer <jwt>, multipart: file=<clip>)
  1. auth extractor → user_id  (JWT Supabase, sinon 401)
  2. rate-limit (tower-governor, clé = user_id)
  3. lire les bytes multipart → fichier temp
  4. whisper_push_core::load_audio_file(tmp)        # symphonia décode m4a/aac → 16kHz mono f32
  5. whisper_push_core::transcribe_with_backend(&audio, lang, &backend)
  6. dico PER-USER (obligatoire, cf. §3.5) : finalize(text, user_dict[user_id])
  7. → 200 { "text": "...", "ms": 412 }

POST /v1/learn        (Authorization: Bearer <jwt>, json: {raw, finalized, corrected, lang})
  → whisper_push_dict::learn(...) sur le dico du user_id → persiste dans Supabase
  → invalide l'entrée de cache (§3.5)
```
- Modèle **chargé une fois** dans le `static MODEL` global (déjà le cas,
  `transcribe/mod.rs:20`) → worker chaud, instantané dès le 2ᵉ appel.
- **Pas de ffmpeg** : `symphonia` est pur-Rust, compilé dans le binaire (`Cargo.toml:34`
  a déjà `aac`). Le téléphone envoie son `.m4a` brut, tout le décodage+resampling est
  serveur.

### 3.3 Écarté (et pourquoi)
- **whisper.cpp `examples/server`** : marche, mais sérialise tout derrière un mutex
  global, exige `--convert` + ffmpeg pour les formats compressés, et **jette votre code
  Rust** (decode/dico). Non.
- **faster-whisper (CTranslate2)** : ~4× plus rapide sur NVIDIA, mais c'est du **Python**
  → abandon de tout le Rust, 2ᵉ format de modèle. À reconsidérer seulement si débit
  NVIDIA massif plus tard.

### 3.4 Latence (chiffres corrigés)
⚠️ Le mythe « 1,3 s même sur H100 » est un **artefact de cold-start** d'un bench. Vrai
compute `large-v3-turbo` **à chaud** : **dizaines de ms** sur GPU ; ~0,2–0,75 s sur **L4**
pour un clip court. **Le vrai ennemi = le cold-start du scale-to-zero** (2 s optimisé →
42 s non optimisé). Réseau mobile ≈ 50–150 ms, négligeable.

### 3.5 Dictionnaire PER-USER (décidé : ON, obligatoire)
Le dico adaptatif (`whisper_push_dict`) est **activé côté serveur**, donc **forcément
per-user** (un worker est partagé → les corrections d'un user ne doivent JAMAIS fuiter
chez un autre). Design :
- **Stockage** : un `dictionary.toml` (ou ligne JSON/Postgres) **par user** dans le
  nouveau projet **Supabase** (table `dictionaries(user_id, toml, updated_at)` ou bucket
  storage). Source de vérité unique, multi-device.
- **Cache worker** : `LRU<user_id, Arc<Compiled>>` en RAM (le `Compiled` est exactement
  la forme hot-path du plan dico, §3.3 de `adaptive-dictation-plan.md`). Miss → charger
  depuis Supabase → compiler → mettre en cache. Hit → `finalize` en ~0,2 ms.
- **Apprentissage** : `POST /v1/learn` (raw/finalized/corrected) → `learn()` sur le dico
  du user → réécrit Supabase → invalide l'entrée LRU. C'est le pendant cloud du flux
  « corriger la dernière dictée ».
- **Cohérence avec le desktop** : le desktop garde son dico **local** (plan existant).
  Sync desktop↔cloud = *hors scope v1*, à noter pour plus tard (le format `.toml` est
  identique → sync triviale ensuite).
- **Isolation** : la clé de cache et de stockage est **toujours** le `user_id` du JWT —
  jamais d'état global partagé (le `finalize_and_record` à état local du desktop n'est
  PAS réutilisé tel quel côté serveur).

### 3.6 Auth & hébergement
- **Auth** : **nouveau projet Supabase dédié** (créé par toi). Le serveur valide le
  **JWT Supabase** (vérif via JWKS/clé du projet), `user_id = claim sub`. Extractor axum
  → 401 sinon. `tower-governor` pour le rate-limit (clé = user_id → quota par user).
- **Hébergement** : **Modal** ou **RunPod** serverless, GPU **L4/T4** (~0,40–0,80 $/h,
  facturé à la seconde, scale-to-zero). Garder **1 worker chaud** (Modal `min_containers=1`
  / `scaledown_window`, RunPod active worker + FlashBoot) pour tuer le cold-start si la
  latence du 1ᵉʳ mot compte.

---

## 4. Client Expo (partagé iOS + Android)

- **Capture** : `expo-audio` (`useAudioRecorder` + `RecordingPresets.HIGH_QUALITY`) →
  `.m4a`/AAC par défaut. Ne **pas** resampler côté client. Background recording possible
  via `enableBackgroundRecording` (ajoute `UIBackgroundModes: ['audio']`) — nécessite un
  **dev build** (prebuild), pas Expo Go.
- **Upload** : `FileSystem.uploadAsync` ou `fetch(FormData{ uri, name, type:'audio/m4a' })`
  vers `/v1/transcribe`, header `Authorization: Bearer`.
- **Sortie** : `expo-clipboard.setStringAsync` (écriture = aucune permission iOS) +
  rendu du texte dans l'app.
- **Dictionnaire** : soit serveur (§3.5), soit port JS de `finalize()` (cheap, ~0,2 ms).
  Reco v1 : côté serveur, désactivé au début.
- **Toolchain** : Expo **SDK 53+**, **Xcode 16**, CocoaPods 1.16.2, **dev build / prebuild
  (CNG)** obligatoire dès qu'on ajoute audio-background / extensions / App Intents.

---

## 5. iOS — extension clavier (natif Swift) — *milestone v2*

- **Outil** : `@bacons/apple-targets` (`npx create-target keyboard`) → cible
  `UIInputViewController` **Swift** (pas de RN), liée au build via `expo prebuild`.
  App Group auto-mirroré depuis `app.json`
  (`ios.entitlements['com.apple.security.application-groups']`).
- **Flux « Flow Session »** (imposé par le no-mic du clavier) :
  1. bouton micro sur le clavier → ouvre l'app conteneur (URL scheme)
  2. l'app (expo-audio) enregistre → POST `/v1/transcribe`
  3. l'app écrit le texte dans le **conteneur App Group** + **Darwin notification**
  4. le clavier lit le texte → `insertText:` dans le champ courant
- **Pré-requis** : « Allow Full Access » (réseau + App Group). ⚠️ Certaines apps
  (banques, password managers) bloquent les claviers tiers — fallback clipboard (§7).
- **Limites assumées** : pas de micro dans l'extension, cap ~48 Mo, swipe manuel iOS 26.4.

---

## 6. Android — IME (natif Kotlin) — *milestone v3*

- **Cible** : `InputMethodService` (Kotlin) ajoutée via config plugin
  (`withAndroidManifest` injecte `<service BIND_INPUT_METHOD>` + intent-filter
  `android.view.InputMethod` + `@xml/method`) + module Expo local pour les fichiers
  natifs. (RN ne peut pas rendre dans un `Service` → UI clavier en `View` Android.)
- **Flux** (plus simple, pas d'app-hop) : l'IME enregistre directement (`RECORD_AUDIO`,
  `foregroundServiceType="microphone"` + `FOREGROUND_SERVICE_MICROPHONE` sur Android 14+,
  tant que la vue clavier est visible) → POST → `commitText(text, 1)`.
- **Permission** : un `Service` ne peut pas afficher la pop-up runtime → la demander
  depuis une Activity compagnon (l'onboarding Expo) avant.
- **Bonus futur** : Android pourrait redevenir **100 % local** (whisper.cpp dans l'IME,
  cf. FUTO/Transcribro) — hors scope vu la décision « mobile = cloud », mais noté.

---

## 7. Phasing — le chemin de moindre résistance

> Clé : **livrer un produit utilisable SANS le travail natif douloureux du clavier**,
> puis ajouter l'insertion inline. C'est exactement le fallback que Wispr documente
> (« Quick Dictation to Clipboard »).

1. **Phase 0 — Serveur** *(le débloqueur)* : extraire `whisper-push-core`, écrire
   `whisper-push-server` (axum + **JWT Supabase** + rate-limit + **dico per-user**
   LRU/Supabase + `/v1/transcribe` + `/v1/learn`), déployer sur Modal/RunPod.
   *Testable au `curl` ; mesurer la latence réelle L4 chaud/froid + le bench modèle (déc. 4).*
2. **Phase 1 — App Expo standalone (iOS + Android)** : auth Supabase, record → POST →
   **clipboard auto-copy** + Share Sheet + (iOS) **App Intent / bouton Action** « dictée
   rapide », + flux « corriger la dernière dictée » → `/v1/learn`.
   **100 % Expo, ship rapide, zéro clavier natif.** C'est un vrai produit.
3. **Phase 2 — Clavier iOS** (Swift, `@bacons/apple-targets`) : insertion inline,
   pattern Flow Session. *Le gros milestone natif — iPhone prioritaire.*
4. **Phase 3 — IME Android** (Kotlin) : insertion inline, enregistrement direct.
5. **Phase 4 — Polish** : passe de reformatage (LLM optionnel façon Wispr),
   facturation/quotas, streaming, sync dico desktop↔cloud si voulu.

---

## 8. Décisions

1. ✅ **v1 = clipboard-only (Phase 1) avant le clavier (Phase 2)** — produit en semaines,
   valide le pipeline + la latence avant le natif. (= app standalone : parler → texte
   auto-copié dans le presse-papier → coller ; vs clavier = insertion inline plus tard.)
2. ✅ **Auth = nouveau projet Supabase dédié** (créé par toi) → JWT. Pas de réutilisation
   de l'identité Kasar.
3. ✅ **Dictionnaire serveur = ON, per-user obligatoire** (§3.5), stocké dans Supabase,
   caché en LRU par `user_id`, appris via `POST /v1/learn`.
4. ⏳ **Modèle serveur** : `large-v3-turbo-q5_0` + CUDA par défaut ; comparer turbo pleine
   précision (qualité) et Parakeet (déjà dans le repo, vitesse EN) — **à trancher au bench
   Phase 0**.
5. ⏳ **Warm worker épinglé** (~575 $/mo une L4 24/7) **vs** scale-to-zero + fenêtre idle —
   décidé au vu des mesures de cold-start Phase 0.
6. ⏳ **Pricing** (marché 8–15 $/mo, free ~2 000 mots/sem) — plus tard.

---

## 9. Risques & mitigations

| Risque | Mitigation |
|---|---|
| Refactor casse le desktop | `whisper-push-core` est un *move* mécanique (decode/transcribe déjà isolables) ; CI desktop verte avant de continuer |
| Cold-start cloud (1ᵉʳ mot lent) | warm worker épinglé OU fenêtre idle généreuse ; viser un même worker pour la rafale d'un user |
| Fuite de corrections entre users (dico) | dico off / per-user sur le worker partagé (§3.5) |
| Coût GPU qui dérape | scale-to-zero + L4/T4 ; per-clip à chaud = fraction de centime ; surveiller le ratio idle |
| App Review iOS (clavier qui ouvre l'app pour le micro) | pattern accepté (Wispr est live) ; bien documenter la justification micro/Full Access |
| Politique Play Store (IME + RECORD_AUDIO + cloud) | data-safety form, disclosure claire ; possibilité de mode local Android plus tard |
| Apps qui bloquent les claviers tiers (iOS) | fallback clipboard/Share (Phase 1 le couvre nativement) |
| Positionnement « 100 % local » écorné | message clair : **desktop = local**, **mobile = cloud privé zéro-rétention** (≠ Wispr) |
| RN dans le clavier/IME (tentation) | interdit (I3) : surface native minimale, logique dans app/serveur |

---

## 10. Comment Wispr Flow fait (réf concurrent, faits corrigés)

- **iOS** = app conteneur (micro, « Flow Session ») + extension clavier (`insertText:`) +
  App Group. Tap-to-start/stop (pas hold). Full Access obligatoire.
- **Backend 100 % cloud.** ⚠️ ASR = **ensemble par langue via Soniox + Baseten** (les
  noms « Scribe/Gemini/Whisper » qu'on lit partout sont cités par Wispr *en benchmark*,
  pas comme leurs moteurs — leur page subprocessors officielle liste Soniox + Baseten).
- **Vrai IP propriétaire** = passe LLM **Llama 3.1 fine-tunée** (Baseten, TensorRT-LLM)
  qui optimise le **« zero-edit rate » ~85 %** (pas le WER). Streaming WebSocket, budget
  <700 ms (200 ASR + 200 LLM + 200 réseau).
- **Privacy** ⚠️ : Privacy Mode **OFF par défaut sur Android**, **ON sur desktop**, choix
  à l'onboarding iOS. Incident 2025 (audio + captures d'écran envoyés au cloud), scandale
  d'audit Delve 2026. → **Notre angle : zéro-rétention par défaut.**

---

## 11. Sources (vérifiées)

**iOS clavier / Apple**
- Apple — App Extension Programming Guide (Custom Keyboard, no-mic, Full Access)
- Apple — `RequestsOpenAccess` / Configuring Open Access ; `UITextDocumentProxy`
- `facebook/react-native#31910` (RN crash dans extension clavier, ~48 Mo)
- 9to5Mac — Wispr Flow iPhone hands-on ; docs.wisprflow.ai (Flow keyboard, iOS 26.4)

**Expo / extensions**
- `EvanBacon/expo-apple-targets` (cible `keyboard`/`share`/`action`/`app-intent`, App Groups)
- docs.expo.dev — app extensions, CNG/prebuild, config-plugins, dangerous-mods
- docs.expo.dev — `expo-audio`, `expo-clipboard`
- `ynniv/expo-ios-app-intents` ; `Gustash/react-native-siri-shortcut`

**Android IME**
- developer.android.com — Create an input method ; `InputMethodService` / `InputConnection`
- developer.android.com — FGS types (Android 14), bg-start restrictions (exemption IME)
- `facebook/react-native#7762` (RN ne rend pas dans un `Service`)
- `futo-org/voice-input`, `soupslurpr/Transcribro` (whisper.cpp dans l'IME)

**Serveur / cloud**
- repo : `src/audio/decode.rs:11`, `src/transcribe/mod.rs:20,65`, `Cargo.toml:34`
- `ggml-org/whisper.cpp` examples/server ; `SYSTRAN/faster-whisper`
- `tokio-rs/axum`, `benwis/tower-governor`
- modal.com/pricing + cold-start ; runpod.io serverless (FlashBoot)
- inferencebench.io (latence turbo) ; e2enetworks (L4 ASR bench)

**Wispr backend**
- wisprflow.ai/data-controls, /research/supporting-languages, /post/technical-challenges
- docs.wisprflow.ai/articles/5375461355 (subprocessors : Soniox + Baseten)
- baseten.co/resources/customers/wispr-flow (Llama 3.1 fine-tuné)
