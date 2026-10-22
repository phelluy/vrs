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
