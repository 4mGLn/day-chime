# Day Chime icon

2026-11-03 · built-in image_gen으로 생성한 프로젝트 전용 아이콘입니다.
살구·호박색 rounded tile, 짙은 남색의 종과 시계, 아이보리빛 햇살을 조합했습니다.
원본 PNG의 투명 배경을 유지하며 크기별 PNG를 내보냈습니다.

- `day-chime-icon.png`: 생성 원본, 1254px 정사각형 RGBA.
- `day-chime-{16,32,48,64,128,256,512}.png`: 앱·알림용 파생 크기.
- `../extension/icons/icon-{16,32,48,128}.png`: 확장 프로그램에서 직접 사용하는 파일.

재출력은 프로젝트 루트에서 `python3 scripts/export_icons.py`로 실행합니다(Pillow 필요).
Rust 및 브라우저 실행에는 Python/Pillow가 필요하지 않습니다.

## Generation prompt

Built-in `image_gen` 사용, `transparent_background=true`. 실제 생성 프롬프트:

> Create one polished production app icon for Day Chime, a gentle attendance and go-home reminder. Use case logo-brand. Square icon, centered rounded-square tile filling about 90 percent of canvas, transparent outside tile. Refined flat vector-like illustration: warm apricot-to-amber subtle gradient tile, a bold deep midnight-indigo bell silhouette incorporating a clean circular clock face, clock hands suggesting end of workday. One small simple ivory sunlight arc above bell or at corner gives daytime warmth. Strong simple geometry, generous negative space, beautiful balanced optical centering, premium friendly desktop productivity app identity. Must remain highly legible at 16px and 32px; no tiny ticks, no clutter, no lettering, no numbers, no words, no watermark, no mockup, no multiple icons, no external drop shadow, no perspective. Crisp smooth edges. Deliver a single large square PNG app icon with genuine transparency around rounded tile, ready for browser extension toolbar and GNOME notifications.
