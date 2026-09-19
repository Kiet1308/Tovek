# Tovek V2 website: content and design record

Reviewed September 19, 2026. Scope: replace the previous `docs/index.html` completely and add a dedicated `docs/changelog.html` release article. Both are static pages that work from GitHub Pages or a local folder.

## History reviewed

The fixed range is `27422b9f8e0aeca0e61db55992930f17a98f4099` (`v0.9.0-beta`) through `465c9dceff1f1d0c925edcc77dec8afaa1baeb37`: 130 commits, including the PR #3 merge. The local range and GitHub remote branch/tag heads were checked. The inventory includes every commit, including documentation, CI and safety refinements; it is not a count of 130 user-facing features. `scripts/build_site_history.py` regenerates the complete, escaped HTML list and JSON inventory from those fixed endpoints.

The article groups the changes by resulting behavior:

| Area | Evidence checked | Publication decision |
|---|---|---|
| Source-like loops and shared tails | PR #3 history, `37136a6`, `742ee0a`, `d313661`, structurer records | Describe iterator, ownership, edge-transfer and close-event checks; distinguish corpus success from universal coverage |
| Types and meaningful names | `4218b22`, `df5be58`, `b82aefe`, `6db2bca`, `b66a186`, naming contracts | Separate recorded names/types from inferred roles; preserve ambiguity |
| Source bindings | `a3a6444`, `c6b46d1`, `fffb4ad`, `source_binding_preservation.md` | Describe bounded binding preservation and lexical/source identity |
| Semantics | `35b1f57`, loop/capture hardening, `b1c8a82`, `b19b86f`, `0cb36b8`, `d35d9ea`, `f59f886`, `d85d890` | Explain concrete effects: lookup ordering, equality, captured reads, assignment addresses and arity |
| UI and tables | `d313661`, `5de361c`, `1f8b603`, `d85d890`, `c08a908`, `e1f1131` | Rebuild only eligible unobserved constructors; preserve meaningful import callees |
| Expressions and helpers | `fbe21a3`, `3a49cce`, `2add294`, `401c393`, `65dc97f`, `f5fb317`, `a2048e5` | Distinguish bounded recovery, optional synthesis, helper placement, discard proof and retained snapshots |
| Provenance and verification | `5e9c5d6`, `6ee7f8b`, `87061f3`, `70a6fce`, `d4a859e`, `d07b166`, `df80251`, `9b7d1a1`, `fffb4ad`, `f1059d9` | Keep metadata optional and unknown explicit; do not equate AST similarity with equivalence |
| Offline registry | `e12ee84`, `source_registry.md` | Label matched upstream source separately from decompilation; mention pins, licenses and exact recompile checks |
| Performance and tooling | `7346891`, `d1a8a43`, `61e578c`, `1f72676`, `98d2fbb`, `86ebac2` | Describe concrete profiling/cache work; no general speedup or automatic PGO claim |
| AI status | `fd8fc68`, README and user constraints | AI remains disabled; no models or service calls introduced |
| Final output fixes | `c08a908` through `465c9dc`, `roadmap_v2_fix_implementation.md` | All F1–F8 changes represented; Geometry/prettyPrint limitations retained |

Bytecode-version coverage, the Rust implementation, parallel folder processing and the existing local server are described as product capabilities, not invented as new V2 features. Old website performance and competitor-superiority claims were not carried forward. Public release tags show no V2 binary at review time: build/source CTAs point to `roadmap-v2`, with beta binaries explicitly labeled.

## Measurements and examples

The sole source of headline comparison numbers is [final delivery acceptance](roadmap_v2_acceptance/fix_final_delivery.json). The executable was built from `a2048e5`; `465c9dc` is the subsequent documentation/acceptance commit. The history is pinned before this website redesign, avoiding a moving release denominator.

- Generated p/v bindings: 54,058 → 36,826 on 3,975 commonly parsed files, a 31.9% reduction. This is not author-name accuracy.
- Public parse/recompile: 513/513 profiles from 171 sources at O0/O1/O2. Raw mean 0.8265 → 0.8660 on 405 common measured profiles; 108 unknown. All 59 raw / 61 normalized regressions are disclosed.
- Runtime: 138 → 198 passing profiles on the shared 198-profile set. Expanded V2 suite: 246/246. No full private-game execution claim.
- Private output: 3,978 files including 42 empty inputs; 382 → 98 long lines. Total lines grow from 493,350 to 508,870; compact-comment savings are not included in default output.
- 1,050 primary workspace tests (including 761 AST tests), one child repeat; 126 Python tests last run at F7 with those sources unchanged afterward.

Landing code pairs are labeled **illustrative, shortened examples**. They are newly authored explanations of the verified transformations, not private source excerpts or claims that those exact snippets are corpus output. The article's import relay is also a shortened illustration. No private bytecode/source/output is part of the website.

## Design direction

Visual thesis: the landing page is a light editorial poster with graphite typography, an acid-green accent and an original ribbed reconstruction sculpture; the release article switches to an immersive black-and-blue star field with quiet, narrow typography.

Content plan: brand and sculpture → product promise → interactive output examples → four concrete strengths → compact evidence → build/source CTA. The article carries the detailed history, charts, optional-mode distinctions, limitations and sources.

Interaction thesis: staged hero entrance and pointer/scroll response; scroll reveals; keyboard-operable example/metric tabs; a draggable, keyboard-rotatable star-field numeral with pause/reset. Reduced-motion preferences pause animation, and offscreen/hidden-page animation stops.

The user supplied [OpenAI's Astra introduction](https://openai.com/index/gpt-6-astra/) as the article design reference. Its live layout was inspected: deep black/blue field, interactive central star numeral, flanking product labels, narrow centered article and restrained chart frames. Tovek uses original procedural graphics and its own copy/branding; no OpenAI logos, font files or page assets are copied. [lua.expert docs](https://lua.expert/docs), checked the same day, support only the hosted API workflow description, not a quality or speed ranking.

Fonts are self-hosted Inter under the bundled OFL license. The Latin WOFF2 subset retains genuine variable weight (100–900) and optical-size axes. Article paragraphs are white, regular-weight 17px text with approximately 28px leading on desktop; the font filename and stylesheet version prevent reuse of an earlier incorrect thin-font asset. Inter is an independently licensed substitute, not OpenAI Sans. The graphic assets are original SVG; the star field is native Canvas. No package runtime, CDN, trackers, uploads or AI services are required by these pages. The previous medal attribution and memorial are retained.

## Verification

- Edge browser: checked both pages at 320×800, 768×1024 and 1440×900, with an additional 390px visual pass. No horizontal document overflow at any checked width; the sculpture loaded at every size. The 320px hero uses stacked copy to avoid collisions.
- Visually checked the new landing sculpture, mobile navigation and hero, dark release article, white paragraph typography and charts. Computed article paragraph color is `rgb(255, 255, 255)`; font axes were checked directly in the WOFF2.
- Output example tabs: mouse selection, arrow-key navigation and Home update the selected tab and code. Chart tabs switch the displayed metric and values, including keyboard navigation to the shared runtime results.
- History: all 130 unique commit links are present in static HTML. A naming query returns three matching commits, an unmatched query shows the empty state, clearing restores the list, and Show more expands 12 rows to 24.
- Star field: pause/resume, keyboard rotation and reset respond correctly. Reduced-motion behavior and static-content fallback were reviewed in source; system reduced-motion emulation and a JavaScript-disabled browser session were not part of this pass.
- Both copy controls report successful clipboard writes. Browser warning/error log inspection returned no entries during these interaction checks.
- Static checks: valid local targets for 61 links/assets, valid HTML fragment targets, no duplicate IDs, one H1 and an explicit language on each page, JavaScript syntax, deterministic generation of the pinned 130-commit inventory, and `git diff --check`.

This is a website-only change. Decompiler/runtime suites were not rerun; their existing acceptance results are cited as product evidence, not as tests of this redesign. The pages were tested locally; pushing the `roadmap-v2` branch does not assert that the public Pages deployment or a V2 binary release has changed.
