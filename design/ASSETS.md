# Fig Files visual assets

All files are local to this app. No Orange artwork, messaging artwork, or legacy Xcode assets are retained.

| Asset | Purpose |
| --- | --- |
| `icon.png` | 1024 × 1024 opaque app icon. Cargo Piston generates platform-specific icon sizes from this master. |
| `ios/FigFilesLaunchIcon.png` | 256 × 256 launch-screen derivative of the app icon. |
| `ios/FigFilesLaunchScreen.storyboard` | Adaptive iPhone/iPad launch screen with the icon and app name. |
| `resources/brand/app_icon.svg` | Compact vector brand icon used by CHK. |
| `resources/brand/logo.svg` | Transparent folder-and-leaf brand mark. |
| `resources/brand/wordmark.svg` | Font-independent, outlined Fig Files wordmark. |
| `resources/brand/empty-files.svg` | Files empty state. |
| `resources/brand/empty-favorites.svg` | Favorites empty state. |
| `resources/brand/empty-trash.svg` | Trash empty state. |
| `resources/brand/error.svg` | App-specific error illustration. |
| `resources/icons/*.svg` | Folder, file, favorite, recent, and trash icons mapped to CHK's existing icon names. |
| `resources/file-icons/*.{svg,png}` | Nine full-color folder, document, image, video, audio, archive, recent, favorites, and trash icons. SVG sources use layered solid highlights; 256px PNGs are rendered with `resvg`. |

The artwork pairs electric violet `#7C3AED` and luminous purple `#9B5CFF` with chartreuse `#D7FF62`, fresh lime `#A6F347`, and a hot-pink `#FF64BC` fig detail. The icon uses a bold glossy folder-and-fig silhouette on chartreuse. The vector logo, empty states, and file artwork carry the same colors and motif. The interface uses CHK's standard dark theme with the chartreuse accent, and displays full-color file artwork through CHK's image and avatar APIs. Button finishes, surfaces, and navigation tinting follow CHK's standard rendering. There are no direct Pelican UI/PTSD imports or custom rendering overrides. Small UI artwork uses simple SVG paths with solid fills because the runtime does not support SVG gradients. Built-in generic CHK assets remain available through the dependency's fallback resources.

The raster icon was generated using the built-in imagegen tool on October 29, 2026. It was copied into the project and resized with macOS `sips`; no CLI/API fallback was used. The original generated output is retained by the generation tool. Runtime SVGs are editable source assets, not converted bitmap images.

## Exact image-generation prompt

> Redesign the attached Fig Files app icon. Use case: logo-brand. Asset type: production native app icon, exactly 1024 by 1024, full-bleed opaque. The user finds the original dull. Make a striking, joyful, modern icon with a strong unforgettable silhouette and vibrant colors. Keep the idea of a file folder with an integrated fig leaf but boldly redesign it: chunky crisp rounded folder in saturated electric violet #7C3AED and luminous purple #9B5CFF, a fresh bright lime-green fig leaf #A6F347 on the upper tab, and one clean pale-lilac paper peeking from the folder. Subtle hot-pink #FF64BC highlight along folder front edge. Folder front has a small bold stylized half-fig symbol with pink interior and just a few cream seeds, elegantly suggesting the Fig brand. Rich smooth glossy 2.5D surfaces, simplified beautiful geometry, confident rounded shapes, sharp clean edges, tactile depth but no grain, no dull clay. Straight-on composition with slight dimensional depth. Full-bleed brilliant chartreuse #D7FF62 background, no beige. Folder large, centered, fills 75 percent of square, balanced margins, crisp subtle shadow. Vibrant high contrast, legible at small icon sizes, sophisticated playful productivity app. No lettering, no text, no watermark, no outer rounded-square frame, no extra objects, no environment, no mockup. Output the finished icon artwork itself.
