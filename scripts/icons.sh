#!/bin/sh
# Regenerates the raster icons from assets/logo.png, the master art.
# Needs ImageMagick 7 (`magick`), `cwebp` and `pngquant`.
#
# The master is 1254 px square; its rings reach 516 px from the centre. The
# monochrome and notification icons are vectors of the same four rings
# (centres 162.5 px off the middle on both axes, radius 235.5 px, band 101 px),
# scaled by hand; see their XML.
set -eu
cd "$(dirname "$0")/.."
master=assets/logo.png
background='#0B0D1F' # @color/launcher_background
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# Adaptive-icon foreground, 108dp at 4x. The rings reach 28dp (112 px) from
# the centre, inside the 33dp safe zone; the glow fades past it.
magick "$master" -filter Lanczos -resize 272x272 -background none -gravity center -extent 432x432 "$tmp/foreground.png"
cwebp -quiet -lossless -z 9 "$tmp/foreground.png" -o android/app/src/main/res/drawable-nodpi/ic_launcher_foreground.webp

# The app's toolbar mark in the dark scheme: the glowing rings themselves, cropped to the
# rings and their glow, on transparent (the light scheme uses drawable/ic_brand_mark.xml).
mkdir -p android/app/src/main/res/drawable-night-nodpi
magick "$master" -gravity center -crop 1000x1000+-3+1 +repage -filter Lanczos -resize 128x128 "$tmp/mark.png"
cwebp -quiet -lossless -z 9 "$tmp/mark.png" -o android/app/src/main/res/drawable-night-nodpi/ic_brand_mark.webp

# The search page's icon: the launcher icon's visible 72dp, rounded; 128 px
# covers a 32 px tab icon at 4x. Quantised: 8 KB instead of 57, 48 dB PSNR.
magick -size 432x432 "xc:$background" "$tmp/foreground.png" -composite -gravity center -crop 288x288+0+0 +repage \
    \( -size 288x288 xc:black -fill white -draw 'roundrectangle 0,0 287,287 64,64' \) \
    -alpha off -compose CopyOpacity -composite -filter Lanczos -resize 128x128 -strip "$tmp/icon.png"
pngquant --quality 85-98 --speed 1 --strip --force --output crates/omnid/src/ui/icon.png "$tmp/icon.png"
