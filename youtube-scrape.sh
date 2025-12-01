#!/bin/sh
root=$(dirname "$0")
# get comma seperated list of subscriptions from newline seperated file
subs=$(paste -s -d, <"$root/youtube-subs.txt")
# download all feeds
curl "https://www.youtube.com/feeds/videos.xml?channel_id={$subs}" >"$root/youtube-feeds.xml"
