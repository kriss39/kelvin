# Brand

| File | Use |
|---|---|
| `kelvin-mark.svg` | The mark: a K drawn on a scale (thermometer, block-height ruler) whose ticks warm from zero at the bottom. Favicon and app icon. |
| `kelvin-logo-light.png`, `kelvin-logo-dark.png` | Mark with wordmark and tagline, for light and dark backgrounds. |
| `og.png` | 1200×630 link preview, served by the API at `/og.png`. |

Wordmark: **kelvin**, lowercase like the SI unit, Archivo at 125% width and weight 800.
Tagline: *Rialo history, down to the last kelvin.*

| Colour | Hex (light / dark) | Meaning |
|---|---|---|
| Ink | `#0e1d29` / `#e3ecf2` | Text, the mark's tile |
| Ice | `#0a6c8c` / `#52c2e0` | Kelvin, indexed data, links |
| Heat | `#a8501a` / `#e59a5e` | The RPC's limits, pruned or missing data |
| Paper | `#eef2f5` / `#0a1116` | Background |

**Thermal ramp**, cold to hot: `#15aabf` → `#4c6ef5` → `#7048e8` → `#e64980` → `#f76707` → `#f59f00`.
Used for the mark's ticks, the bar under the header, the hero accent and activity heat (user
transactions per block). Elsewhere colour encodes meaning, not decoration.

**Program families** (light / dark), the same in every chip, row stripe and instruction:
System `#0b7285`/`#3bc9db`, Compute Budget `#64748b`/`#94a3b8`, Oracle `#6741d9`/`#9775fa`,
Token `#a16207`/`#fcc419`, Associated Token `#c2410c`/`#ff922b`, Tokenomics `#2b8a3e`/`#69db7c`,
any other program `#c2255c`/`#f783ac`.

`logo.html` and `og.html` are the sources of the PNGs; render them with a headless browser at
760×200 (transparent) and 1200×630.
