#!/bin/sh
# Runs every fuzz target for SECONDS each (default 60) with the limits of QA-1:
# 10 s per input, 2 GiB RSS. New inputs go to corpus/<target> (ignored by git);
# seeds/<target> is read as extra seeds, and the repository's .alto samples and
# test fonts are copied into the corpus of the targets that read them.
# TARGETS="path font" limits the run. Requires a nightly toolchain and cargo-fuzz;
# FUZZ_TOOLCHAIN picks the nightly (default `nightly`).
set -u
cd "$(dirname "$0")" || exit 1
seconds=${1:-60}
toolchain=${FUZZ_TOOLCHAIN:-nightly}
targets=${TARGETS:-$(cargo "+$toolchain" fuzz list)} || exit 1
if [ -z "$targets" ]; then
  echo "no fuzz targets" >&2
  exit 1
fi
status=0
for t in $targets; do
  mkdir -p "corpus/$t"
  case $t in
    package | recovery) cp ../tests/fixtures/*.alto ../verification/*.alto "corpus/$t/" || exit 1 ;;
    # folder の corpus にコピーするのは薄い .alto の 1 件だけ（seeds/folder と
    # 併用）。decode_folder は外部履歴を持つ入力を UNSUPPORTED として拒否する
    # ため、この seed は folder の拒否経路を試す。大きな .alto を混ぜないのは
    # 入力上限の増加と試験速度のため。正常な folder 経路は
    # seeds/folder/sample.alto（17,831 バイト）で試す。
    folder) cp ../tests/fixtures/thin-versions.alto "corpus/$t/" || exit 1 ;;
    font) cp ../tests/fonts/*.otf ../tests/fonts/*.ttf "corpus/$t/" || exit 1 ;;
  esac
  echo "== $t"
  if [ -d "seeds/$t" ]; then
    set -- "corpus/$t" "seeds/$t"
  else
    set -- "corpus/$t"
  fi
  cargo "+$toolchain" fuzz run "$t" "$@" -- \
    -max_total_time="$seconds" -timeout=10 -rss_limit_mb=2048 -print_final_stats=1 \
    || status=1
done
exit $status
