Le but de ce projet est de créer une sorte d'éditeur intégré pour l'apprentissage de la programmation avec distraction minimale. Le code doit être écrit en rust avec egui pour l'interface.

Le look est similaire à vscode quand on a ouvert un dossier

- à gauche un panneau (mince) pour choisir des fichiers, créer/supprimer des fichiers, des dossiers, déplacer à la souris les dossiers et les fichiers
- au milieu un seul panneau d'éditeur de texte avec coloration syntaxique
- en bas un terminal dans lequel on peut lancer des compilations et des runs.
- pas de menu en haut, tout se fait avec sélection et clic et clic droit avec menu contextuel
- quand on clique sur un fichier à gauche il se charge dans l'éditeur en remplçant le fichier édité (avec offre de sauvegarde du fichier en cours d'édition)

tout doit être autocontenu dans un seul paquet (en particulier un compilateur c++ avec les libs de base)

keep it simple and stupid, on recherche le minimalisme pas plusieurs façon de faire la même chose.

## Décisions (22/10/2026)

- Compilateur : celui du système (g++), pas de toolchain embarqué. La phrase
  "autocontenu dans un seul paquet" signifie : un seul binaire applicatif,
  sans dépendance à un runtime tiers.
- Terminal : vrai terminal interactif (PTY) lançant le shell du système.
  L'étudiant tape lui-même les commandes de compilation — l'apprentissage du
  terminal est un but, donc pas de bouton "compiler" ni de commande de build
  intégrée.
- Cible : Linux d'abord. Pas de version WebAssembly (le navigateur ne peut
  pas lancer g++ ni un shell). egui est multi-plateforme ; seule la couche PTY
  (portable-pty) est à adapter pour Windows/macOS plus tard.
- Projets : dans ~/.vrs (chaque sous-dossier = un projet).
- Éditeur : egui_code_editor, thème clair, fonctionnalités minimales
  (numéros de ligne, undo/redo).
- Interface : en anglais, structurée pour permettre l'ajout de langues plus tard.
- Hors périmètre pour l'instant : onglets multiples, git, debugueur,
  extensions, auto-complétion, parsing des diagnostics du compilateur
  (clic sur une erreur pour sauter à la ligne — envisagé plus tard).

## clangd : options envisagées (23/10/2026)

Constat : `/usr/bin/clangd` existe (Apple clangd 21.0.0, arm64, fourni par les
Command Line Tools). On garde cette version système, pas de `llvm` Homebrew.
Deux outils de la chaîne LLVM sont donc de versions différentes
(`clang-format` 23.1.4 via Homebrew, clangd 21) : sans conséquence, chacun
fait son travail.

Ce que clangd apporterait que `g++ -fsyntax-only` ne donne pas : plages de
soulignement exactes, notes et suggestions dans le message, diagnostics sur
le buffer non sauvegardé sans passer par un fichier temporaire, hover réel
sur les identifiants, aller à la définition. En contrepartie : un protocole
(LSP) à parler, un processus persistant à gérer, et une dépendance système
de plus.

### Essai sans code

```sh
clangd --check=~/.vrs/projet/hello.cpp
```

Sort les diagnostics et les flags déduits, sans éditeur ni protocole. À faire
avant d'écrire quoi que ce soit, pour comparer avec la sortie de `g++`.

### Prérequis communs aux options qui parlent LSP

- JSON-RPC sur stdio, trames `Content-Length: N\r\n\r\n{…}`. Soit à la main
  avec `serde_json` seul, soit avec `lsp-server` + `lsp-types`.
- Synchronisation en *full text* : on renvoie tout le buffer à `didChange`.
  Les deltas incrémentaux ne sont pas nécessaires et compliquent tout.
- Une instance par dossier de projet (une sous-dossier de `~/.vrs`), arrêt
  propre (`shutdown`, `exit`, kill) au changement de projet et à la fermeture.
- Un `.clangd` (dans `~/.vrs` ou par projet) pour retrouver les flags actuels,
  que clangd n'utilise pas par défaut :
  ```yaml
  CompileFlags:
    Add: [-Wall, -Wextra]
  ```

### Option A — clangd remplace le linter

`Linter` garde sa forme (thread, canal, `update()` par frame), seul le
lancement change : un clangd persistant au lieu d'un `g++` par frappe.
`publishDiagnostics` remplace `parse()` ; `Diagnostic` reçoit `end_line` et
`end_column` pour souligner la plage réelle au lieu de deviner la fin du mot.
Disparaissent : l'anti-rebond maison (clangd gère son propre délai) et la
copie cachée `.vrs-lint-*` (voir `temp_path`), puisque le texte part par
`didChange`.

Coût : ~250-350 lignes dans `diagnostics.rs`. Risque : les messages ne sont
plus littéralement ceux de la compilation de l'étudiant, ce qui contredit
l'argument actuel du module (« exactly the errors the student will meet »).

### Option B — hybride

Garder `g++` pour les diagnostics, ajouter clangd uniquement pour ce que le
compilateur ne fait pas :

- hover sur identifiant (`textDocument/hover`), ~60 lignes : `draw_diagnostics`
  sait déjà convertir une position en glyphe, donc en offset ;
- aller à la définition (`textDocument/definition`) sur Ctrl+clic, ~60 lignes :
  la réponse est un chemin plus une plage, et `Editor::open` / `set_cursor`
  existent déjà.

Coût : deux processus à gérer, mais aucune régression sur les diagnostics et
aucune réécriture de `diagnostics.rs`. C'est l'option à préférer si l'option A
doit être évaluée avant d'être adoptée.

### Option C — statu quo

Rien n'est fait. On peut alors se contenter du point « envisagé plus tard » de
la liste ci-dessus : clic sur une erreur pour sauter à la ligne, sans clangd.

### Écarté pour rester minimaliste

- Auto-complétion (`textDocument/completion`) : hors périmètre assumé, et
  c'est le plus gros travail d'interface (popup ancré au curseur, filtré à la
  frappe).
- Coloration sémantique (`semanticTokens`) : concurrence directe avec
  `egui_code_editor` et `syntax.rs`, gain faible.
- `clang-tidy` : diagnostics de style dont un débutant ne sait rien faire.

## Décision du 23/10/2026 : option A, plus hover et définition

Option A retenue, avec les deux fonctions de l'option B : puisque le serveur
est là, autant lui demander aussi le survol et le saut à la définition.

- `src/clangd.rs` (nouveau) : le transport LSP — trames `Content-Length`,
  `initialize`/`initialized`, `didOpen`/`didChange` (texte complet)/
  `didClose`, `hover`, `definition`, `publishDiagnostics`. Deux fils par
  serveur (écriture vers stdin, lecture de stdout) ; les réponses passent par
  un canal et réveillent egui (`request_repaint`), sinon l'éditeur resterait
  inerte une fois la frappe finie.
- `src/diagnostics.rs` devient `src/language.rs` : un clangd par projet (le
  dossier juste sous `~/.vrs`), arrêt propre au changement de projet et à la
  fermeture. `Diagnostic` gagne `end_line`/`end_column` : le soulignement suit
  la plage exacte du serveur au lieu de deviner la fin du mot. Les
  diagnostics publiés pour d'autres fichiers (en-têtes inclus) sont ignorés.
- `src/editor.rs` : survol après 0,35 s d'immobilité du pointeur (une requête
  par frame saturerait le serveur), Ctrl+clic sur un identifiant (Cmd+clic
  sur macOS, où Ctrl+clic est le clic droit habituel : même règle que
  Ctrl+S, qui devient Cmd+S). Un saut hors de `~/.vrs`
  est ignoré : atterrir dans un en-tête système remplirait l'éditeur de
  milliers de lignes et relancerait un serveur sur un autre projet. Avant un
  saut vers un autre fichier, le tampon courant est enregistré.
- `~/.vrs/.clangd` est écrit une fois s'il manque, pour retrouver
  `-Wall -Wextra` : clangd n'active aucune alerte par défaut. Un `.clangd` de
  projet s'y ajoute.
- Une dépendance de plus : `serde_json`, et rien d'autre.

Disparaissent : l'anti-rebond maison, la copie cachée `.vrs-lint-*` et
l'appel à `g++` pour les diagnostics. `g++` reste indispensable au terminal :
c'est l'étudiant qui compile. Écart assumé, comme prévu : les messages sont
ceux de clang et non plus ceux de g++ (libellés voisins, `[-W…]` conservé en
fin de message).

Reste ouvert : clic sur une erreur pour sauter à la ligne. clangd donne déjà
la plage, il ne manque que le clic.
