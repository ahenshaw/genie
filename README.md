# Genie

A desktop app for viewing, building and editing family trees stored as GEDCOM files.

![A person's profile](docs/profile.png)

![Ancestors and descendants in the Tree view](docs/tree.png)

![The Graph view](docs/graph.png)

## Requirements

- Rust 1.95 or newer ([rustup.rs](https://rustup.rs))
- On Linux, file dialogs use the XDG desktop portal (`xdg-desktop-portal`), which most desktops already have.

## Build and run

```sh
git clone https://github.com/ahenshaw/genie.git
cd genie
cargo run --release
```

To open a file directly:

```sh
cargo run --release -- path/to/family.ged
```

## Getting started

- **Open a file:** File → Open… (Ctrl+O), or drag a `.ged` file onto the window.
- **Try the sample:** click *Explore the sample* on the start screen, or File → Open sample tree.
- **Start a new tree:** File → New tree (Ctrl+N) and add the first person. Add parents, partners, children and siblings from the *Family* card on their profile, or from the dashed **+** slots in the Tree view.
- **Edit someone:** select them and press Ctrl+E, or click *Edit* on their profile.
- **See how two people are related:** right-click someone and choose *Relationship to …*, or open the Relationship section and pick both people.
- **Add documents and photos:** drop files onto the window to attach them to the selected person, or use *Add* on their profile. Files are copied into a `<tree name> media` folder next to the tree file. Save a new tree before adding documents.
- **Clean up an imported tree:** *Tools → Find duplicate people…* lists people who may have been entered twice and lets you compare and merge them; *Find duplicate sources…* merges repeated sources.
- **Set a home person:** right-click someone and choose *Set as home person*. Each profile then shows how that person is related to them.
- **Save:** Ctrl+S, or Ctrl+Shift+S to save as a new file.

## Sections

| Section  | What it shows |
|----------|---------------|
| Profile  | The selected person's details, life events, sources, family, and a small family map |
| Tree     | Ancestors, descendants, or both around the selected person |
| Relationship | The line of descent connecting two people, and what the relationship is called |
| Media    | Every document and photo in the tree, with filters for unattached and missing files |
| Graph    | People and families as connected nodes that can be edited by wiring them together, either through family nodes or directly from parent to child |
| Overview | Counts, common surnames and places, and people missing key details |
| GEDCOM   | The selected person's records as they will be saved |

## Keyboard shortcuts

| Keys | Action |
|------|--------|
| Ctrl+N | New tree |
| Ctrl+O | Open |
| Ctrl+S / Ctrl+Shift+S | Save / Save as |
| Ctrl+Z / Ctrl+Shift+Z | Undo / Redo |
| Ctrl+F | Find a person |
| Ctrl+P | Add a person |
| Ctrl+E | Edit the selected person |
| Ctrl+Enter | Save in the editor |
| Alt+← / Alt+→ | Back / Forward |
| Alt+Home | Go to the home person |
| Ctrl+1 … Ctrl+7 | Switch section |

In the Tree view, drag to pan and Ctrl+scroll to zoom.

## Tests

```sh
cargo test
```
