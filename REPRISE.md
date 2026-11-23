# Reprise — Mémoire (arrêt du 23/11 au soir)

**Fait** : docs lus, questions posées et réponses reçues (ci-dessous), dépôt git local, `Cargo.toml` (egui/winit/softbuffer comme Vigie, socle v0.1.6, chacha20poly1305), `src/crypt.rs` (clé aléatoire protégée par DPAPI + ChaCha20-Poly1305 par enregistrement). Rien ne compile encore : pas de `main.rs`. Les 3 sous-agents (secrets+transformations, OCR WinRT, capture GDI) ont été arrêtés avant de livrer ; `wip/` est vide et ignoré par git.

**Reste** (plan) : É1 surveillance presse-papiers (fenêtre message-only + `AddClipboardFormatListener`, raccourcis, icône de zone de notification) + `store.rs` (journal chiffré en ajout seul, compaction, rétention) + fenêtre de recherche `socle::fuzzy` · É2 secrets, exclusions, coller transformé · É3 capture (overlay GDI multi-écran PMv2, puis annotation egui) · É4 OCR `Windows.Media.Ocr` en fond · É5 `--stats-semaine --json`, STACK/README, release. Relancer les 3 briefs des sous-agents (dans le transcript de la session du 23/11).

**Réponses de l'utilisateur** :
1. Historique : **Alt+V** ; si l'app au premier plan l'utilise, ne rien forcer ; repli configurable dans les réglages.
2. Capture : Ctrl+Maj+S. 3. Démarrage avec Windows + icône dans la zone de notification.
4. Conservation : 6 mois pour le texte, 3 mois pour les images, 2 Go au total, réglable ; épingles et collections jamais purgées. 5. Fichiers : chemin seulement.
6. Choisir = copier, puis Ctrl+V à la main ; pas de « coller tout seul » en v1.
7. Secrets **gardés mais masqués** (•••• jusqu'au clic), chiffrés, exclus de l'OCR, de la recherche plein texte et des stats ; effacés du presse-papiers Windows après 30 s si c'est simple (l'entrée reste dans l'historique).
8. Exclure la navigation privée + liste perso vide au départ. 9. Barre d'annotation (Entrée copier, Ctrl+S enregistrer).
10. OCR sur les captures et les images copiées, en français et en anglais. 11. Vidéo/GIF plus tard.
12. Tout en local ; `--stats-semaine --json` dès la v1 : `{"v":1,"debut":"AAAA-MM-JJ","copies":n,"captures":n,"par_jour":[7 entiers]}`, option `--semaine AAAA-MM-JJ`.

Dernière consigne du chef : aller vite, un seul passage de `cargo test`, audit et EGM plus tard, puis publier et installer dans `dist\` via explorer.exe.
