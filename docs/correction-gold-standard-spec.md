# Cahier des charges — Correction gold standard (phonétique + sémantique, sans LLM lourd)

> Synthèse d'une veille multi-agents (GitHub / arXiv / produits, 42 techniques évaluées).
> Objectif : atteindre le niveau Wispr Flow / Aqua Voice **sans second modèle lourd**, 100 % local,
> latence ~instantanée, agnostique au backend là où c'est possible.

## 0. Principe directeur — le canal bruité

Toute la correction est `argmax_term  P(term) · P(heard | term)` :
- `P(heard | term)` = ressemblance **phonétique + orthographique** (Levenshtein + fold/Metaphone). *(on l'a déjà)*
- `P(term)` = a priori. Aujourd'hui **plat/unigramme** → le trou. Le passer à **`P(term | contexte)`** = toute la dimension **sémantique**, et ça se fait **sans LLM** (contexte de session + co-occurrence + n-grammes).

## 1. Invariants (rappel, non négociables)
- **I1** hot path ~0 (≤ 0,2 ms, court-circuit dico vide).
- **I2** un seul chemin POST-traitement (`finalize`) commun aux 3 backends. Le biasing décodage est un **accélérateur Whisper-only optionnel**, jamais la stratégie commune.
- **I3** jamais casser un mot juste (garde mots-courants + seuils + proper-noun-only + confiance + démotion).
- **I4** réversible, listable, éditable.

## 2. Faits établis par la veille (contraintes dures)
- **Post-traitement = seule couche vraiment agnostique.** Parakeet (`parakeet-rs` greedy-only, decode_with_beam_search = stub) et Voxtral (burn) **n'exposent aucun hook** de décodage. Le biasing décodage est **Whisper-only**.
- **Whisper expose DÉJÀ (whisper-rs 0.16, zéro nouvelle dép) :**
  - `set_filter_logits_callback` → **logit-boost** (trie de tokens BPE, +~3 en log-space sur les continuations) — *prévient* l'erreur.
  - `token_probability()` / `token_data().p` → **confiance par token** — corriger seulement les mots peu sûrs.
  - `set_initial_prompt` / `set_grammar` (grammaire = flaky en dictée libre → rejetée).
- **Le « sémantique » des produits (Wispr, Aqua, Superwhisper, Apple) n'est PAS un LLM de correction** : c'est du **contexte de session** (Accessibilité : texte de l'app active, sélection, presse-papier, Contacts) injecté comme vocabulaire transitoire prioritaire.
- **Le vrai n-best / rescoring neuronal est REJETÉ** : indisponible uniformément (Voxtral = 0 alternative), et tout correcteur neuronal (HyPoradise, Whispering-LLaMA, PMF-CEC, SpellMapper-reranker) = le « second modèle » refusé. On en **emprunte les idées algorithmiques**, pas les modèles.

## 3. Architecture cible — couches (du plus universel au plus spécifique)

```
                      ┌─────────────────────────────────────────────┐
HotkeyDown ──────────►│ L0  Contexte de session (transient vocab)   │  AX: app/sélection/presse-papier
                      │     → termes prioritaires ★ pour CETTE dictée│  (macOS; dégradé clipboard ailleurs)
                      └─────────────────────────────────────────────┘
audio → MODÈLE ──┬───►[Whisper] L1  logit-boost décodage (feature, Whisper-only)  ← MÊME liste de boost
                 │                + confiance par token (Option<f32> par mot)
                 ▼
            texte brut + Vec<(mot, Option<f32> confiance)>
                 │
                 ▼  finalize()  (POST, agnostique, choke point unique)
   ┌─────────────────────────────────────────────────────────────────────────┐
   │ L2 EXACT n-gram (déterministe, garantie)                                  │
   │ L3 FLOU = génération de candidats (blocking phonétique) → RESCORING       │
   │     linéaire sur features :                                               │
   │        • dist. phonétique (fold FR/EN + Metaphone opt.)                    │
   │        • dist. édition (Levenshtein + Jaro-Winkler)                        │
   │        • a priori fréquence  log(count)                                    │
   │        • SCORE CONTEXTE = co-occurrence apprise + n-gramme local          │ ← SÉMANTIQUE
   │        • confiance ASR (élargit le seuil sur les mots peu sûrs)           │
   │     + garde retention/confiance (PMF-CEC) : on n'écrase pas un mot sûr    │
   │ L4 Groupes homophones/mot-courant : correction AUTORISÉE seulement si     │ ← SÉMANTIQUE
   │     le contexte (L0 session OU co-occurrence) vote pour un membre         │
   └─────────────────────────────────────────────────────────────────────────┘
                 │
                 ▼  apprentissage (auto-capture édition + classifier) → met à jour : variantes,
                    table de co-occurrence, compteurs, n-grammes locaux  (boucle fermée)
```

## 4. Spécifications par couche

### L0 — Contexte de session (★ ROI le plus élevé, agnostique)
- À `HotkeyDown` : récupérer (macOS, Accessibilité déjà câblée) `AXValue` de l'élément focus, `AXSelectedText`, nom de l'app au premier plan, presse-papier. Linux/Windows : presse-papier + titre fenêtre.
- Extraire les candidats **proper-noun-like** (Majuscule interne/initiale, hors mots-courants), les injecter comme **`FuzzyTerm` transitoires ★** dans un **overlay `Compiled`** valable **pour cette seule dictée**.
- Réutilise les gardes `accept_fuzzy` existantes. **Aucune écriture disque** (transitoire).
- **Acceptation** : un nom présent dans la sélection/clipboard et mal transcrit est corrigé sans avoir été appris au préalable.

### L1 — Décodage Whisper (feature `whisper-bias`, Whisper-only)
- **Logit-boost** : trie de tokens BPE construit depuis la **boost-list** (cap ~200, classée ★ > contexte-session > fréquence > récence) ; `set_filter_logits_callback` ajoute `+cb_weight (~3.0)` aux tokens qui continuent un chemin actif, annulation sur cul-de-sac (algo TCPGen/CTC-WS **sans la tête neuronale**).
- **Confiance** : dans `transcribe_whisper`, lire `token_probability()` et émettre `Vec<(mot, Some(conf))>`. Parakeet/Voxtral → `None` (dégradation = comportement actuel).
- **Acceptation** : un terme de la boost-list mal entendu sort correct du décodeur (mesuré sur corpus audio L4) ; aucun impact latence mesurable.

### L2 — Exact (inchangé) : garantie « jamais 2× la même erreur ».

### L3 — Flou rescoré (refonte `accept_fuzzy` → `score_candidate`)
- **Génération de candidats** : index de **blocking par clé phonétique** (`HashMap<fold_key, Vec<FuzzyTerm>>`) → ne Levenshtein que le même bucket (corrige le O(tokens×|dico|), garde I1 à grande échelle). Upgrade futur : automate de Levenshtein via crate `fst`.
- **Score linéaire** (forme Apple 2506.06117), poids constants nommés & calibrés sur le corpus golden :
  `score = w_p·sim_phon + w_e·sim_edit + w_f·log1p(count) + w_c·score_contexte + w_conf·gate_confiance`
  Accept si `score ≥ seuil` **et** gardes I3.
- **Gating confiance** (Whisper) : mot à haute confiance → seuil relevé (quasi intouchable) ; mot à basse confiance → seuil abaissé. `None` → comportement actuel.
- **Phonétique** : fold multilingue **+ règles FR** (voyelles nasales an/en/in/on, finales muettes, gn→ny, eau/au→o, ph→f, c/g doux-durs) sélectionnées via `lang`. `rphonetic` (Double Metaphone/Beider-Morse) en **feature optionnelle**, A/B sur le corpus.
- **Acceptation** : précision ≥ aujourd'hui sur le corpus de pièges ; rappel en hausse sur les variantes FR ; latence ≤ 0,2 ms.

### L4 — Désambiguïsation homophones / mots-courants (le trou sémantique)
- Groupes d'équivalence (clé = fold/Metaphone) : `their/there`, `Marc/mark`, `mer/maire/mère`…
- Correction **dans la classe mot-courant** autorisée **uniquement** si **L0 (session)** ou la **table de co-occurrence** vote pour un membre. Sinon, garde mot-courant comme aujourd'hui.
- **Acceptation** : « envoie à **Marc** » corrigé quand « Marc » est un contact/visible ; « laisse une **mark** » non touché en contexte neutre.

### Score contexte (la SÉMANTIQUE sans modèle)
- **Co-occurrence** : table `HashMap<terme, HashMap<cue, poids>>` apprise depuis la **même** boucle d'auto-capture (les mots autour d'une correction deviennent des indices). Sub-ms, zéro modèle.
- **n-gramme local** : trigramme **stupid-backoff** pur Rust (feature opt.), **semé depuis les dictées passées de l'utilisateur** + petit corpus embarqué → tie-break `log(count(w-2,w-1,cand))`. Stockage bincode/`fst`, dizaines de Ko–Mo.

## 5. Dépendances (lean préservé)
| Crate | Usage | Statut |
|---|---|---|
| `strsim` | Jaro-Winkler co-signal (+ Levenshtein) | léger, à ajouter |
| `rphonetic` | Double Metaphone / Beider-Morse | **feature opt.** ; fold-FR pur par défaut |
| `fst` | automate Levenshtein + n-gramme à grande échelle | **feature opt.** (V1.1) |
| `aho-corasick` | bias-retrieval top-k | opt. (grand dico) |
Le chemin chaud par défaut reste `std` + fold-maison + Levenshtein-maison. **Pas** de KenLM FFI, **pas** de modèle neuronal.

## 6. Matrice backend
| Couche | Whisper | Parakeet | Voxtral |
|---|---|---|---|
| L0 contexte session | ✅ | ✅ | ✅ |
| L1 logit-boost | ✅ (feature) | ❌ (greedy ONNX) | ❌ (fork) |
| L1 confiance | ✅ | ❌→`None` | ❌→`None` |
| L2/L3/L4 post | ✅ | ✅ | ✅ |

## 7. Rejeté (et pourquoi)
GBNF grammaire (gibberish en dictée libre) · n-best/lattice (indispo uniforme) · correcteurs neuronaux HyPoradise/Whispering-LLaMA/PMF-CEC/SpellMapper-reranker (= second modèle refusé) · KenLM FFI (C++/cmake) · Metaphone3 (commercial) · biasing décodage Parakeet/Voxtral (pas de hook ; fork lourd). **On en garde les idées** (trie+boost, gate sélectif KEEP/CHANGE, garde de rétention), pas les modèles.

## 8. Plan d'implémentation (ordre ROI/risque)
1. **API confiance** : `finalize(Vec<(word, Option<f32>)>, lang)` + plomberie confiance Whisper. *(agnostique, testable)*
2. **A priori fréquence** (tie-break `count`) + **blocking phonétique**. *(trivial→scaling, testable)*
3. **Phonétique FR** dans `fold()` + **Jaro-Winkler** co-signal. *(testable)*
4. **Score contexte par co-occurrence** + refonte `score_candidate` linéaire. *(la sémantique, testable)*
5. **L0 contexte de session** (AX scrape → overlay transient). *(ROI max, AX)*
6. **L4 homophones** gated par contexte. *(testable)*
7. **L1 logit-boost Whisper** (feature `whisper-bias`). *(accélérateur)*
8. *(opt.)* trigramme stupid-backoff, `fst`, `rphonetic` A/B.

Chaque étape : tests golden verts + scorecard `dict_eval` (precision/rappel) + bench latence, avant la suivante. Critère global : **précision ≥ actuelle sur les pièges** (faux positifs = péché capital) **et** rappel + UX en hausse.

## 9. Reste à faire — plan détaillé (post-acoustique)

> Fait : phonétique FR + a priori fréquence, contexte de session, recall renforcé termes utilisateur,
> **dictionnaire acoustique** (A→D, audité). Reste, ordonné par valeur (agnostique d'abord) :

**F — Co-occurrence (sémantique, agnostique) [le cœur « sens »]**
- `Entry.context: Vec<String>` (serde, dans dictionary.toml). À l'apprentissage d'un terme, on enregistre
  les mots-indices = voisins non-courants (`left_ctx`/`right_ctx`).
- `FuzzyTerm.context` (cues normalisées). `finalize` calcule l'ensemble des mots-contenu de la dictée ;
  si un cue d'un candidat y est présent → seuil **boosté** (le sens soutient la correction).
- Audit : cas golden — cue présent ⇒ corrige ; contexte neutre ⇒ ne corrige pas.

**G — Homophones/mots-courants gardés par contexte [comble le trou sémantique]**
- Groupes d'équivalence (clé fold/metaphone). Correction DANS la classe mot-courant autorisée **seulement**
  si le contexte (session L0 OU co-occurrence F) vote pour un membre. Sinon garde mot-courant.
- Audit : « envoie à Marc » (Marc en contexte) ⇒ corrige ; « laisse une mark » neutre ⇒ intact.

**H — Enrichissement internet cold-path (opt-in, demandé) [jamais sur le hot path]**
- À l'APPRENTISSAGE d'un nom propre seulement : requête Wikipedia/Wikidata (ureq léger) pour valider
  l'orthographe canonique. Toggle config **défaut OFF**, offline-safe, caché. Jamais par dictée.
- Audit : terme tech mal orthographié → canonique ; offline → no-op ; toggle off → zéro réseau.

**I — Apprentissage acoustique depuis l'édition in-place [auto-capture]**
- Câbler `acoustic::learn_word` dans l'auto-capture (pas que le panneau). Historique court des dictées
  récentes (audio+timings) clé = texte finalisé, pour empreindre le bon audio malgré le décalage temporel.
- Audit : logs + repli panneau (déjà fiable).

**J — Timings token Whisper réels + rebuild final**
- `set_token_timestamps` + lecture t0/t1 par token → spans mots précis (vs énergie). Puis audit
  adversarial final + rebuild + réinstall DMG avec F→J.

**Différé (faible valeur pour Parakeet) :** confiance par token (Whisper-only) ; logit-boost Whisper (feature).
