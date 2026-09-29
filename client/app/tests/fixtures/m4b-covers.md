The M4B cover fixtures contain 0.1 seconds of generated silent audio, with
title `The Hobbit` and artist `JRR Tolkien`. No audiobook audio is included.
`embedded-cover.m4b` additionally contains a generated red 60 × 90 PNG.
`square-cover.m4b` uses a red 90 × 90 PNG instead.

Generated with FFmpeg:

```sh
ffmpeg -f lavfi -i anullsrc=r=8000:cl=mono -t 0.1 -c:a aac \
  -metadata title='The Hobbit' -metadata artist='JRR Tolkien' -f mp4 coverless.m4b
ffmpeg -f lavfi -i color=c=red:s=60x90 -frames:v 1 cover.png
ffmpeg -i coverless.m4b -i cover.png -map 0:a -map 1:v -c copy \
  -disposition:v attached_pic -f mp4 embedded-cover.m4b
ffmpeg -f lavfi -i color=c=red:s=90x90 -frames:v 1 square.png
ffmpeg -i coverless.m4b -i square.png -map 0:a -map 1:v -c copy \
  -disposition:v attached_pic -f mp4 square-cover.m4b
```
