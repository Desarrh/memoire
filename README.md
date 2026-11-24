# Mémoire

A long-memory clipboard and screenshot tool for Windows, written in Rust. It is a single portable exe, it runs without admin rights, and everything stays local.

- **Clipboard history kept for months**: text, images and copied files (by path). Fuzzy search covers the text and the text recognised in images. You can pin entries.
- **Screenshots** with **Ctrl+Shift+S**: drag a region, click a window, or press E for a whole screen. Multi-monitor and DPI aware. You can annotate with an arrow, a box or blur. **Enter** copies, **Ctrl+S** saves a PNG to `Pictures\Captures`.
- **Local OCR** (`Windows.Media.Ocr`, French and English) runs in the background at low priority on every capture and copied image.
- **Paste as** pretty or compact JSON, UPPER/lower/Title case, Windows ↔ Unix paths, trimmed or one-line text. Text copied back from the history is always plain.
- **Privacy**:
  - Detected secrets (tokens, keys, passwords) are kept masked (••••) until you reveal them. They are excluded from search and stats, and wiped from the Windows clipboard after 30 s.
  - Password managers (`ExcludeClipboardContentFromMonitorProcessing`) and private browsing windows are never recorded.
  - Storage is encrypted: the key is protected by DPAPI and each entry is encrypted with ChaCha20-Poly1305.
- **Alt+V** opens the history (fallback: Ctrl+Shift+V). Win+V and Print Screen are left to Windows.

`Memoire.exe --stats-semaine --json [--semaine YYYY-MM-DD]` prints weekly counts (no content), for example `{"v":1,"debut":"2026-11-23","copies":12,"captures":3,"par_jour":[…]}`.

Build: `cargo build --release`. MIT license.
