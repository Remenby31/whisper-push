# Spec — Dictée adaptative (apprendre des corrections), v4

> Quand l'utilisateur corrige une transcription (noms propres, jargon, mots mal reconnus),
> Whisper Push s'ajuste pour ne plus refaire la même erreur. **100 % local, zéro modèle en
> plus, zéro latence ajoutée, une stratégie uniforme pour tous les backends.**

## ✅ Statut d'implémentation (2026-06-04)

- **Phase A/A2/C — FAIT & testé.** Crate `crates/whisper-push-dict/` (pur, 3 deps) : model,
  normalize, phonetic, compiled, finalize (exact + flou gardé), learn (diff + classifier +
  promotion/démotion). `finalize_and_record` branché en fin de `transcribe_with_backend`
  (agnostique au modèle). Toggle `dictionary_enabled` dans config.
- **CLI — FAIT.** `whisper-push dict {list,add,remove,learn,path}` (voir/gérer/simuler).
- **Corpus golden — FAIT.** 156 cas (`fixtures/*.jsonl`) issus d'un workflow adversarial à
  10 angles, vérifiés contre l'implémentation (`examples/dict_eval.rs`, `--emit`).
  Seuils calibrés : flou phonétique **0.72**, base **0.84**, porte phonétique **0.6**,
  réécriture si `sim_doc<0.5` / >3 spans / **toute** substitution non-phonétique.
- **Reste (Phase B/D) :** UI in-app — sous-menu tray « Dictionnaire » + panneau « Corriger la
  dernière dictée » (`correct_last()`/`last_dictation()` déjà prêts dans le crate). V2 = biais
  d'entrée par modèle.

---

## 0. Invariants (non négociables)

- **I1 — Hot path ~0.** `finalize()` ≤ 0,2 ms ; aucune I/O, aucun LLM, aucune regex, aucune
  allocation superflue. Court-circuit immédiat si le dictionnaire est vide.
- **I2 — Uniforme & agnostique.** Une seule logique pour Parakeet / Whisper / Voxtral. Aucun
  `if backend == …` dans la V1.
- **I3 — Ne casse jamais ce qui marche.** La correction *exacte* (issue de l'apprentissage)
  est déterministe et sûre. La correction *floue* est conservatrice, gardée, et réversible.
- **I4 — Tout est réversible.** Toute règle apprise est listable, éditable, supprimable, et
  démotable par feedback négatif.
- **I5 — Robustesse aux verrous empoisonnés** : `lock().unwrap_or_else(|e| e.into_inner())`
  partout (cf. note "poisoned lock" du projet).

---

## 1. Deux garanties, deux natures de correction

| | **Exact** (variant → terme) | **Flou / phonétique** (terme connu) |
|---|---|---|
| Rôle | **garantie** : « jamais 2× la même erreur » | **généralisation** : variantes jamais vues |
| Source | appris depuis tes corrections, ou saisi | dérivé des termes du dico |
| Risque | nul (déterministe) | faible (gardé : mot-courant + seuil strict) |
| Couverture | la forme exacte déjà corrigée | formes proches non encore vues |

L'exact porte la promesse produit ; le flou n'est qu'un bonus conservateur. Même si le flou
ne généralise presque rien, la boucle d'apprentissage garantit le « never twice » via l'exact.

---

## 2. Layout fichiers / modules

**Le cerveau vit dans un crate workspace LÉGER** (cf. §17.0 — condition de la boucle de test
serrée), `whisper-push` n'y dépend que par `path` :
```
Cargo.toml                       # [workspace] members = ["crates/*"] ; package racine
crates/whisper-push-dict/        # crate PUR (deps : serde, toml, strsim, unicode-normalization, similar, [rphonetic])
  src/
    lib.rs        # API : load/reload, finalize, finalize_traced, learn, management
    model.rs      # Entry, Dictionary, Source, (de)serde TOML
    compiled.rs   # Compiled (tables hot-path) + build()
    normalize.rs  # normalize(), tokenizer (Word/Sep), reconstruction
    finalize.rs   # exact n-gram + flou phonétique (hot path)
    learn.rs      # diff, classifier, promotion, démotion (cold path)
    phonetic.rs   # clé phonétique + similarité (strsim + metaphone optionnel)
  fixtures/       # corpus golden (§17.2) : finalize.jsonl, learn.jsonl
  examples/dict_eval.rs   # scorecard precision/rappel
src/                       # crate whisper-push (lourd) — n'appelle que whisper_push_dict::{finalize_traced, learn, record_last}
config: <config_dir>/whisper-push/dictionary.toml   # séparé de config.toml
```

Branchements existants :
- `src/transcribe/mod.rs` → appel `dictionary::finalize` en fin de `transcribe_with_backend`.
- `src/tray/mod.rs` `run_pipeline` (~1379) → maj du slot `LastDictation` ; item tray + hotkey
  « Corriger la dernière dictée ».
- `src/config.rs` → quelques toggles (serde `default`, compat préservée).

---

## 3. Modèle de données

### 3.1 `dictionary.toml`
```toml
version = 1

[[entry]]
term = "Kasar"
variants = ["cazar", "kazaar", "caesar"]   # "heard-as" (déjà vues)
starred = true                              # priorité (flou + biais d'entrée V2)
count = 7                                   # nb de fois corrigé/utilisé
undo_count = 0                              # feedback négatif → démotion
source = "manual"                           # "manual" | "auto"
lang = "fr"                                 # optionnel ; None = toutes langues

[[entry]]
term = "Claude Code"
variants = ["cloud code", "clode code"]
starred = false
count = 3
undo_count = 0
source = "auto"
```

### 3.2 Structs
```rust
#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    term: String,
    #[serde(default)] variants: Vec<String>,
    #[serde(default)] starred: bool,
    #[serde(default)] count: u32,
    #[serde(default)] undo_count: u32,
    #[serde(default)] source: Source,      // default = Manual
    #[serde(default)] lang: Option<String>,
}
#[derive(Clone, Copy, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
enum Source { #[default] Manual, Auto }

#[derive(Default, Serialize, Deserialize)]
struct Dictionary { version: u32, #[serde(rename = "entry", default)] entries: Vec<Entry> }
```

### 3.3 Forme compilée (hot path, jamais reconstruite à l'exécution)
```rust
struct Compiled {
    // exact : clé = n-gram normalisé ("cloud code") → terme canonical ("Claude Code")
    exact: HashMap<String, Arc<str>>,
    max_ngram: usize,                       // = max mots parmi toutes les variantes, capé à 4
    // flou : termes "corrigeables", proper-noun-like, avec leur clé phonétique
    fuzzy: Vec<FuzzyTerm>,                  // { term: Arc<str>, norm: String, meta: Option<String>, starred: bool }
    common_words: Arc<CommonWords>,         // lexique mots-courants par langue (garde-fou flou)
    empty: bool,                            // court-circuit I1
}
```

### 3.4 Partage & rechargement
`static DICT: OnceLock<RwLock<Arc<Compiled>>>`. Le hot path fait `DICT.read().clone()` (clone
d'`Arc`, ~lock-free, pas de recompilation). L'écriture (apprentissage/édition) reconstruit un
`Compiled` neuf et swap l'`Arc`. (Option : `arc_swap::ArcSwap<Compiled>` pour des reads sans
lock — petite dép ; sinon `RwLock<Arc<…>>` suffit, zéro dép.)

---

## 4. Normalisation & tokenisation (`normalize.rs`)

### 4.1 `normalize(s) -> String`
1. minuscule Unicode (`to_lowercase`),
2. décomposition **NFD** puis suppression des marques combinantes (accents) — `é`→`e`,
3. trim. → utilisé pour **les clés exactes** et la **similarité** du flou/classifier.

### 4.2 Tokeniseur préservant la mise en forme
On découpe en suite de `Tok` pour pouvoir **reconstruire à l'identique** le texte non modifié :
```rust
enum Tok { Word(String), Sep(String) }   // Word = run \p{Alphabetic}|\p{Nd}|marks ; Sep = le reste
```
- `"cazar, voici Kasar."` → `[Word("cazar"), Sep(", "), Word("voici"), Sep(" "), Word("Kasar"), Sep(".")]`
- Reconstruction = concaténation. Un remplacement multi-mots consomme les `Sep` internes du span
  et émet le terme canonique (espaces canoniques). Apostrophe/trait d'union = `Sep` en V1
  (limite connue pour les termes à trait d'union → cf. §13).

---

## 5. `finalize(text, lang) -> String` (hot path)

```
si Compiled.empty → return text                         // I1 court-circuit
toks = tokenize(text)
out = String
i = 0
tant que i < toks.len():
    si toks[i] == Word:
        # 5a. EXACT, longest-match d'abord
        matched = None
        pour L de min(max_ngram, mots_restants) descendant à 1:
            key = normalize(join_words(toks[i..], L))     # ignore les Sep internes
            si exact.contains(key): matched = (L, exact[key]); break
        si matched:
            out.push_str(&terme) ; i = i_après_L_mots ; continue
        # 5b. FLOU (sur le mot seul ; bigrammes optionnels) — conservateur
        si phonetic_enabled && !common_words.contains(normalize(word), lang):
            best = argmax_{t in fuzzy} sim(normalize(word), t.norm)
            si best && accept_fuzzy(word, best, lang): out.push_str(&best.term) ; i+=1 ; continue
        out.push_str(word) ; i += 1
    sinon: out.push_str(sep) ; i += 1
return out
```

### 5.1 `accept_fuzzy` (garde-fous — le cœur de l'« I3 »)
Accepte le remplacement `word → term` **seulement si toutes** :
- `normalize(word)` **n'est pas** un mot courant de `lang` (tue la classe `their/there`,
  `mark/Marc`),
- `word` n'est pas déjà un terme/variant exact (déjà traité en 5a),
- **similarité** `sim = 1 - lev(norm_word, norm_term)/max(len)` ≥ **seuil**,
  - seuil **0,82** par défaut (strict),
  - si `lang == "en"` **et** Double Metaphone(word) == Double Metaphone(term) → seuil abaissé
    à **0,68** (la concordance phonétique autorise plus de tolérance),
- écart de longueur plausible (`|len_word - len_term| ≤ 3`),
- (option) ne corriger en flou que les termes `starred` ou `source==Auto` "haute confiance"
  pour réduire encore le risque.

Tout est **tunable** via config ; `phonetic_enabled` ON par défaut, mais strict.

### 5.2 Casse
Remplacement = casse **canonique** du terme (`"Kasar"`). Cas limite (jargon bas-de-casse en
début de phrase) accepté en V1 — cf. §13.

### 5.3 Coût
`exact` : O(tokens × max_ngram) lookups HashMap. `fuzzy` : O(tokens × |fuzzy|) Levenshtein
courts — OK pour dicos réalistes (≤ quelques centaines). Au-delà : prune par bucket phonétique
ou `aho-corasick` (§13). Budget ≤ 0,2 ms tenu.

---

## 6. Hot path — intégration & capture pour l'apprentissage

À la fin de `transcribe_with_backend` :
```rust
let raw = text;                                  // sortie brute du modèle
let (finalized, applied) = dictionary::finalize_traced(&raw, language);
dictionary::record_last(LastDictation { raw, finalized: finalized.clone(), applied, lang });
Ok(finalized)
```
- `finalize_traced` renvoie aussi `applied: Vec<Applied { raw_span, term }>` = la liste des
  remplacements faits **cette dictée** (nécessaire au feedback négatif, §9).
- `LastDictation` = slot RAM unique écrasé à chaque dictée (coût nul). `applied` ≈ 0–3 entrées.

---

## 7. Cold path — capture (panneau)

- **Déclencheur** : item tray « Corriger la dernière dictée… » + hotkey configurable.
- **Surface** : mini-fenêtre **SwiftUI** sur macOS (réutilise la cible Swift du wizard
  d'onboarding) : un champ texte pré-rempli avec `finalized`, boutons Enregistrer / Annuler.
- À Enregistrer → `corrected` repart vers le cœur Rust (`dictionary::learn(last, corrected)`).
- **macOS d'abord** (plateforme de distribution). Linux/Windows : apprentissage via l'UI de
  gestion (§10) en attendant un panneau natif. Pas d'Accessibility, pas de monitoring.

> Le **cœur** (`learn` = diff + classifier + promotion) est **pur** et testable sans UI : on
> peut le valider headless avant de câbler le panneau (cf. phasing §12).

---

## 8. Diff (`learn.rs`)

Entrées : `finalized` (vu par l'utilisateur) et `corrected` (voulu).
1. Tokenise les deux en **mots** (la ponctuation isolée est un token ; on garde les indices).
2. **Alignement** par `similar::TextDiff::from_slices(&a_words, &b_words)` → suite d'ops
   `Equal | Delete | Insert`. (Alternative 0-dép : LCS maison ~40 lignes.)
3. **Spans de substitution** : chaque run contigu `Delete*Insert*` encadré d'`Equal` →
   `Span { deleted: Vec<&str>, inserted: Vec<&str>, left_ctx, right_ctx }`.
   Un `Delete` pur = suppression ; un `Insert` pur = ajout (ignorés pour l'apprentissage de
   vocabulaire en V1 — on n'apprend que les substitutions).

---

## 9. Classifier — correction ponctuelle vs réécriture

But : n'apprendre que les **vraies erreurs ASR**, jamais une réécriture de fond.

```rust
enum EditClass { NoChange, PunctualCorrections(Vec<Pair>), Rewrite }
struct Pair { heard: String, corrected: String, demote: bool, left_ctx: String, right_ctx: String }
```

**Niveau document (rejet rapide des réécritures)**
- `sim_doc = mots_inchangés / max(len_a, len_b)`. `sim_doc < 0,5` → **`Rewrite`** (rien appris).
- nombre de spans de substitution `> 3` → réécriture/stylistique → **`Rewrite`**.

**Niveau span (pour chaque substitution restante)**
- taille : `deleted` et `inserted` chacun ≤ **3 mots**, sinon span rejeté.
- **🔑 porte phonétique** : `deleted` et `inserted` doivent **se ressembler à l'oreille** :
  `sim(normalize(del), normalize(ins)) ≥ 0,5` (langue-agnostique) **ou** Double Metaphone égal
  (en). Sinon → changement de fond → span rejeté (PAS appris).
- si le span passe : `Pair { heard = surface ASR du deleted, corrected = inserted, … }`.

**Feedback négatif (démotion)** — alignement 3 voies `raw / finalized / applied / corrected` :
- si le `deleted` correspond à un remplacement de `applied` (donc **notre** correction
  automatique, que l'utilisateur défait) → `Pair.demote = true` sur la règle fautive
  (`undo_count++` ; si `undo_count ≥ 2` et `source==Auto` → retrait de la variante/entrée).
  Et on apprend `raw_span → corrected` (la bonne cible).
- sinon `heard` = le `deleted` (qui égale le raw puisque non touché par finalize).

Seuils centralisés dans une struct `Thresholds` (constantes nommées) → **calibrables** sur un
petit corpus étiqueté (cf. tests §11). Aucun nombre magique dispersé.

---

## 10. Promotion & gestion

**Promotion** (par `Pair` retenue, garde-fou façon Wispr ✨) :
- n'apprend que **noms propres / mots rares** : `corrected` a une majuscule (initiale ou
  interne) **ou** est absent du lexique de mots-courants de `lang` **ou** déjà vu ≥ N fois.
  → évite de polluer avec du vocabulaire courant.
- crée/incrémente l'`Entry` : ajoute `heard` aux `variants` (dédup, normalisé), `count++`,
  `source=Auto` si nouvelle. Reconstruit `Compiled` + sauvegarde atomique `dictionary.toml`.
- `demote` → `undo_count++` / retrait comme ci-dessus.

**UI de gestion (V1)** : sous-menu tray « Dictionnaire… » (ou pane réglages) — lister, ★,
ajouter (terme + variantes), éditer, supprimer, marquer Auto ✨, import/export TOML.

---

## 11. Config (ajouts, serde `default`, compat préservée)
```rust
dictionary_enabled: bool,        // défaut true
phonetic_correction: bool,       // défaut true (flou conservateur)
phonetic_threshold: f32,         // défaut 0.82 (avancé)
correct_last_hotkey: String,     // défaut "cmd+shift+u" (ex.)
```
Les **entrées** vivent dans `dictionary.toml` ; seuls les **toggles** sont dans `config.toml`.

---

## 12. Dépendances (exactes, justifiées)
| Crate | Usage | Chemin | Note |
|---|---|---|---|
| `strsim` | Levenshtein/ratio | chaud (flou) + froid | minuscule, pure Rust |
| `unicode-normalization` | NFD (accents) | les deux | standard |
| `similar` | diff mot-à-mot | froid | sinon LCS maison (0 dép) |
| `rphonetic` | Double Metaphone (en) | optionnel | sinon metaphone maison ; flou marche sans (Levenshtein seul) |
| `arc-swap` | reads lock-free | optionnel | sinon `RwLock<Arc<…>>` |

Chemin **chaud** strict : `std` (HashMap) + `strsim` + `unicode-normalization`. Le reste est froid/optionnel.

---

## 13. Tests (concrets, headless)
- `normalize` : `"Café"→"cafe"`, casse, marques.
- tokeniseur : round-trip `reconstruct(tokenize(s)) == s` (fuzz court).
- **exact** : `"cazar."→"Kasar."` ; multi-mots `"cloud code"→"Claude Code"` ; longest-match
  (`"cloud"` seul non touché si seule `"cloud code"` est une variante) ; casse/ponctuation.
- **flou** : `"kazaar"→"Kasar"` (positif) ; **garde-fou** `"their"`/`"there"` jamais touchés ;
  `"marc"` non transformé en `"Mark"` si `marc`/`mark` sont communs ; seuil respecté.
- **diff** : extraction de spans (substitution simple, multiple, insertion/suppression pures).
- **classifier** : correction ponctuelle acceptée ; réécriture (`sim_doc<0,5`) → `Rewrite` ;
  changement de fond phonétiquement éloigné → span rejeté ; calibration sur ~20–30 paires
  réelles annotées (corpus de test) → mesurer précision/rappel.
- **promotion/démotion** : proper-noun accepté, mot courant refusé, `undo_count` incrémenté
  quand l'utilisateur défait une auto-correction.
- **perf** : `finalize` sur dico de 300 entrées × phrase 30 mots < 0,2 ms (bench `criterion`
  optionnel ou simple `Instant`).

---

## 14. Phasing
1. **Phase A — cœur exact** : `model.rs` + `compiled.rs` + `normalize.rs` + `finalize.rs`
   (exact seul d'abord), branchement 1 ligne, `dictionary.toml`, tests exacts. *Livrable :
   correction déterministe agnostique, latence ~0.*
2. **Phase A2 — flou** : `phonetic.rs` + `accept_fuzzy` + garde-fous + tests faux-positifs.
3. **Phase C — cerveau d'apprentissage (pur, headless)** : `learn.rs` (diff + classifier +
   promotion + démotion) + `LastDictation` + tests/corpus. *Testable sans UI.*
4. **Phase B — panneau** « Corriger la dernière dictée » (SwiftUI macOS) + hotkey/tray.
5. **Phase D — UI gestion** dictionnaire (lister/éditer/supprimer/import-export).
6. **V1.1** — démotion auto affinée ; bucket/aho-corasick si dico volumineux.
7. **V2+** — biais d'**entrée** par modèle (Whisper `set_initial_prompt` sans fork ; fork
   Voxtral ; word-boosting Parakeet), via trait `WordListBias` (défaut no-op), même liste commune.

---

## 15. Risques & mitigations
| Risque | Mitigation |
|---|---|
| Flou casse un mot juste | garde-fou mot-courant + seuil strict + proper-noun-only + démotion + toggle OFF |
| Phonétique anglo-centré (FR) | Levenshtein normalisé = signal **primaire** (langue-agnostique) ; metaphone = bonus en |
| Apprendre une réécriture | classifier `sim_doc`/nb-spans + porte phonétique |
| Dico volumineux → latence | court-circuit vide ; bucket phonétique / aho-corasick en V1.1 |
| Casse en début de phrase | accepté V1 ; option « préserver casse de position » plus tard |
| Trait d'union / apostrophe | `Sep` en V1 ; étendre la classe `Word` si besoin |
| Verrou empoisonné | `unwrap_or_else(into_inner)` partout (I5) |
| Multi-fenêtre / focus (panneau) | hand-off via la cible Swift existante (wizard) |

---

## 16. Décisions à valider ensemble
1. **Flou hot-path ON par défaut** (conservateur) vs **exact-only** (flou en opt-in) ?
   → reco : **ON conservateur** (c'est lui qui donne l'effet « intelligent » multi-modèles).
2. **Panneau de capture = macOS-only en V1** (Linux/Win via UI de gestion) ? → reco : **oui**.
3. **Construire le cerveau (Phase C) headless avant le panneau (Phase B)** ? → reco : **oui**.
4. **Deps** : `strsim` + `unicode-normalization` + `similar` (+ `rphonetic`/`arc-swap` opt) →
   OK ? → reco : oui ; metaphone & arc-swap restent optionnels.
5. **Hotkey** « corriger la dernière dictée » par défaut (`cmd+shift+u` ?).

---

## 17. Test autonome & boucle de feedback minimale

> Principe : **le cerveau est pur sur des chaînes** → testable sans audio, sans modèle, sans
> GUI, sans humain. On pousse tout le risque dans la couche autonome et instantanée ; le seul
> vrai pas humain résiduel = un smoke-test du panneau, **1 fois**.

### 17.0 Levier décisif — isoler le cerveau dans un crate léger
`whisper-rs` (cmake/whisper.cpp), `burn`/`wgpu`, `parakeet-rs`/onnx sont **lourdes et non
optionnelles** dans `whisper-push` → chaque `cargo test` paie ce tax (minutes). On extrait le
cerveau dans `crates/whisper-push-dict/` (deps pures uniquement) :
- `cargo test -p whisper-push-dict` compile en **~1–2 s**, sans jamais toucher whisper.cpp/wgpu/onnx.
- `whisper-push` dépend du crate par `path` et ne fait qu'appeler `finalize_traced`/`learn`.

### 17.1 Couches de test (de la plus serrée à la plus lente)
| Couche | Commande | Humain | Vitesse | Couvre |
|---|---|---|---|---|
| **L1 unit** | `cargo test -p whisper-push-dict` | non | ~1–2 s | normalize, round-trip tokeniseur, exact, garde-fous flou, diff, classifier, promotion |
| **L2 corpus** | golden JSONL chargés en `#[test]` | non | ~1–2 s | qualité sur données variées ; precision/rappel ; **régressions** |
| **L3 session** | `#[test]` de séquence | non | ~1–2 s | comportement **temporel** : apprend → généralise → démote |
| **L4 audio réel** | `whisper-push --transcribe-file x.wav` | non | ~s (inférence) | pipeline modèle→finalize **sans** hotkey/paste/TCC/GUI |
| **L5 panneau** | clic fenêtre SwiftUI | **oui (1×)** | manuel | câblage GUI seul (logique `learn` déjà verte) |

### 17.2 Corpus golden (cœur de l'auto-test qualité)
Deux fichiers que **je rédige** (je sais à quoi ressemble le bon comportement) :
- `fixtures/finalize.jsonl` : `{dict, input, expect}` — corrige bien ? ne casse pas un mot juste ?
- `fixtures/learn.jsonl` : `{raw, finalized, corrected, expect_class, expect_pairs}` — classifier + promotion.
Un `#[test]` les charge et asserte ; `cargo run -p whisper-push-dict --example dict_eval` imprime
le scorecard (precision/rappel, mismatches). Exit ≠ 0 sur régression → loopable.

### 17.3 Boucle autonome que JE fais tourner
```
# inner loop (secondes, zéro humain) :
cargo test -p whisper-push-dict
cargo run -p whisper-push-dict --example dict_eval        # quand je tune des seuils
# périodique (sanity, zéro humain, plus lent) :
say -o /tmp/k.aiff "I met Kasar in Paris"; <→ wav>; \
  cargo run --features metal -- --transcribe-file /tmp/k.wav
```
**Bootstrap autonome du corpus** : je passe quelques phrases `say`→`--transcribe-file` pour
**récolter de vraies mécompréhensions** du modèle, puis je les **fige** dans `finalize.jsonl`
→ rejeu instantané ensuite. Pilotable via `/loop` pour une itération continue.

### 17.4 Où reste l'humain (minimal)
- **1×** : smoke-test du panneau SwiftUI (Phase B) ; la logique derrière est déjà validée L1–L3.
- **optionnel** : relire le scorecard / trancher quelques cas-limites « doit-on apprendre ça ? ».
Tout le reste (finalize, diff, classifier, promotion, généralisation, démotion, pipeline audio)
est **autonome**.

### 17.5 CLI de simulation (remplace l'humain pour la logique)
`whisper-push dict learn --finalized "<txt>" --corrected "<txt>"` → appelle `learn()`, imprime le
diff du dico. Je simule l'édition humaine en passant la chaîne corrigée → chemin d'apprentissage
complet testé **sans GUI ni humain**. (Et `--transcribe-file` pour le pipeline audio.)

---

## Annexes (résumé recherche)
- **Wispr Flow** : ASR cloud + LLM réécriture, **pas de ré-entraînement par user** ; « add
  word » (hint entrée) vs « correct misspelling » (remplacement sortie, caché ≈ `finalize`) ;
  auto-apprentissage limité aux noms propres (✨) ; ~88→96 %+.
- **Écartés** : fine-tuning/LoRA on-device (trop lourd, oubli, données minuscules) ; TCPGen
  (entraînement) ; grammaire GBNF (flaky en dictée libre) ; `initial_prompt` (→ V2 Whisper).

### Sources
- [Wispr — Technical](https://wisprflow.ai/post/technical-challenges) · [Dictionary](https://docs.wisprflow.ai/articles/4052411709-teach-flow-your-words-with-the-dictionary)
- [arXiv 2410.18363](https://arxiv.org/abs/2410.18363) · [CB-Whisper](https://aclanthology.org/2024.lrec-main.262/)
- [whisper.cpp #1979](https://github.com/ggml-org/whisper.cpp/issues/1979) · [grammars #2003](https://github.com/ggml-org/whisper.cpp/discussions/2003)
- [freeflow #125](https://github.com/zachlatta/freeflow/issues/125) · [whisper-rs FullParams](https://docs.rs/whisper-rs/latest/whisper_rs/struct.FullParams.html)
- [`similar`](https://docs.rs/similar) · [`strsim`](https://docs.rs/strsim) · [`rphonetic`](https://docs.rs/rphonetic) · [Voxtral dep](https://github.com/TrevorS/voxtral-mini-realtime-rs)
