#!/bin/sh
# Download the file
url="$1"
size="$2"
tmpfile="/tmp/dlimage-$$.jpg"

curl -L "$url" -o "$tmpfile" -s

# Optionally, you can process the image with convert if needed, or just pass the file to jiv_binary
# If you want to keep the convert step, uncomment the following and adjust as needed:
# convert "$tmpfile" -gravity center -crop 8:4 -resize "$size>" RGBA:- | ./jodie-s-image-viewer/jiv_binary "$size"

# If jiv_binary accepts the file directly:
~/jiv2/target/release/sextant "$tmpfile" --width="$size"

# Clean up
rm -f "$tmpfile"
