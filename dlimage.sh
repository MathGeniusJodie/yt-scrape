#!/bin/sh
curl "$1" | convert - -gravity center -crop 8:4 -resize "$2>" RGBA:- | ./jodie-s-image-viewer/jiv_binary "$2"
