# Mémoire — stack technique (v0.2.0)

## 1. But et direction
Presse-papiers à mémoire longue + capture d'écran, avec OCR local. Remplace Win+V (sans le désactiver) et l'outil Capture. Tout reste local et chiffré.

## 2. Stack
Rust 2024 (rustc 1.99), egui 0.37 + winit 0.30 + softbuffer + `hdrshot-egui-backend` (rendu CPU, comme l'Explorateur et Vigie), `socle` v0.1.6 (ui, settings, crash, instance, testmode), `windows-sys` 0.61 (presse-papiers, raccourcis, zone de notification, GDI, DPAPI, registre), `windows` 0.62 (WinRT `Windows.Media.Ocr`), `chacha20poly1305` 0.10, `chrono` 0.4, `image` (PNG).

## 3. Architecture
| Fichier | Rôle |
|---|---|
| `main.rs` | boucle winit, fenêtre cachée/affichée, capture sur un thread, CLI `--stats-semaine`, démarrage avec Windows |
| `watch.rs` | thread de veille : fenêtre cachée, `AddClipboardFormatListener`, `RegisterHotKey`, icône et menu de la zone de notification, `SetWinEventHook` (premier plan), effacement des secrets à 30 s |
| `clip.rs` | lecture/écriture du presse-papiers (texte, DIB, CF_HDROP), formats d'exclusion |
| `store.rs` | journal chiffré en ajout seul, dédoublonnage, rétention, images PNG chiffrées, statistiques |
| `crypt.rs` | clé protégée par DPAPI + ChaCha20-Poly1305 |
| `app.rs` | interface : historique (recherche `socle::fuzzy`, aperçu, épingles, Ctrl+K, F1) et éditeur d'annotation ; thread OCR en priorité basse |
| `capture.rs` | image figée du bureau virtuel (BitBlt), overlay GDI de sélection (zone, fenêtre, écran) |
| `drag.rs` | glisser-déposer OLE : objet de données du shell (`SHCreateDataObject`) avec CF_UNICODETEXT et CF_HDROP, `SHDoDragDrop` |
| `annotate.rs` | flèche, cadre, flou (pixellisation) |
| `ocr.rs` | `Windows.Media.Ocr`, fr + en, tuiles si l'image dépasse la taille maximale |
| `secrets.rs`, `transform.rs` | détection des secrets, coller transformé |

Threads : interface, veille (messages Win32), OCR (`THREAD_MODE_BACKGROUND_BEGIN`), capture (un par capture), chargement d'aperçu (ponctuel).

## 4. Direction artistique
`socle::theme` (crème / bleu nuit), icônes `socle::icons`, annotations en rouge `#E53B3B`, cadre de sélection `#6FA8FF`.

## 5. Conventions
- Données : `%LOCALAPPDATA%\Memoire\` (`cle.bin`, `journal.bin`, `img\*.bin`, `stats.json`, `glisser\` : PNG temporaires des glisser-déposer, vidé à chaque démarrage). `MEMOIRE_DIR` les remplace (tests).
- Réglages : `%APPDATA%\Memoire\settings.json`. Ils couvrent les raccourcis, `laisser_raccourci` (les exe pour lesquels Alt+V est laissé à l'app), `apps_exclues`, la rétention (183 jours pour le texte, 92 pour les images, 2048 Mo), `demarrage_windows`, `effacer_secrets_30s`, `ocr` et `fermer_apres_copie` (faux : la fenêtre reste ouverte après une copie).
- Démarrage avec Windows : la valeur `Memoire` dans `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, qui lance `"<exe>" --demarrage`.
- Ressource de version `MemoireCli = "stats-semaine"`, que Bilan peut lire avant d'appeler l'exe.
- En mode test (`SOCLE_TEST=1`), l'app ne pose ni veille du presse-papiers, ni raccourcis, ni icône, ni entrée dans le registre, et ne fait pas de capture.

## 6. Fonctionnalités
L'historique s'ouvre avec Alt+V, ou avec le raccourci de secours si Alt+V est pris. On y trouve les filtres Tout, Texte, Liens, Images, Fichiers et Épinglés, la recherche floue et les touches ↑↓, Entrée (copie, toast « Copié », la fenêtre reste ouverte), Alt+1…9 (copier la n-ième ligne), Ctrl+clic / Maj+clic (multi-sélection : textes fusionnés, ou fichiers), Ctrl+P, Suppr, Ctrl+R (révéler un secret), Ctrl+Maj+C (texte d'une image), Ctrl+E (afficher dans l'explorateur), Ctrl+T (premier plan), Ctrl+K et F1. Glisser une ligne vers une app : texte, chemin(s) de fichier, ou image en PNG. La fenêtre se ferme avec Échap, l'icône, ou quand elle perd le focus (sauf au premier plan).
Les images ont une miniature (cache chiffré `img*.t.bin`, 360 px), et leur aperçu agrandi s’affiche au survol d’une ligne ; un texte long s’affiche aussi au survol.
Capture avec Ctrl+Maj+S. Dans l'overlay, F choisit une fenêtre, E un écran et R une zone. L'éditeur propose les outils A, R et B, ainsi que Ctrl+Z, Entrée, Ctrl+S et Échap.

Mesuré : 17 Mo de RAM privée au démarrage en mode test, CPU nul au repos, OCR d'une image 1920 × 1080 en ~170 ms, 28 tests (dont `historique_en_colonne`, le bug de la v0.1.0 : lignes posées côte à côte, une seule visible).

## 7. Intégrations
Bilan : `Memoire.exe --stats-semaine --json [--semaine AAAA-MM-JJ]` → `{"v":1,"debut","copies","captures","par_jour":[7]}`. `par_jour` compte les copies et les captures, sans les secrets.

## 8. Limites connues
- Pas encore vérifiés à l'écran (règle §0) : le glisser-déposer vers les autres apps, la multi-sélection à la souris, la zone de notification et les raccourcis.
- Pas de collections dans la v1, seulement des épingles. Pas de mise à jour automatique.
- Un texte de plus de 1 Mo est tronqué. La recherche porte sur les 2 000 premiers caractères de chaque entrée.
- Alt+V est enregistré globalement. Si l'app au premier plan s'en sert, il faut l'ajouter à `laisser_raccourci`.
- La détection de la navigation privée se fait sur le titre de la fenêtre.

## 9. Pistes QoL (à valider)
Collections, coller automatique (en option), capture vidéo/GIF, recapturer la même zone, et ouvrir une capture dans le lecteur.
