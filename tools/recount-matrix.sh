#!/usr/bin/env bash
# recount-matrix.sh — recount PORT-MATRIX.md frozen-table per-symbol rows and
# verify the "Current" header table agrees (Gate C remediation, 2026-11-03:
# hand-maintained header counts had drifted ±1 from the rows).
#
# Only frozen-table rows count: `| `symbol` | **1**|2 | source | owner | status… |`
# Supplementary tables (markdown/keymap extras) have a non-numeric tier column
# and are excluded by design.
#
# Usage: tools/recount-matrix.sh          # recount + compare header (exit 1 on drift)
set -euo pipefail
cd "$(dirname "$0")/.."

awk -F'|' '
function trim(s){ gsub(/^[ \t]+|[ \t]+$/,"",s); return s }
/^\| `/ {
  sym=trim($2); tier=trim($3); owner=trim($5); st=trim($6)
  t=tier; gsub(/[* ]/,"",t)
  if (t!="1" && t!="2") next                      # not a frozen-table row
  split(st,w,/[ \t(\[]/); s=w[1]
  if (s!="done" && s!="partial" && s!="deferred" && s!="todo") {
    printf "AMBIGUOUS line %d: status column starts with %q\n", NR, s > "/dev/stderr"; bad=1; next
  }
  src=trim($4)
  pkg = (src ~ /keymap/) ? "keymap" : (src ~ /solid/ || (src=="—" && tolower($5) ~ /solid/)) ? "solid" : "core"
  cnt[pkg,s]++; tot[s]++
  if (t=="1") t1[s]++
  rows++
}
END {
  if (bad) exit 2
  split("core solid keymap",P," "); split("done partial deferred todo",S," ")
  for(p=1;p<=3;p++){ printf "%s", P[p]; e=0
    for(q=1;q<=4;q++){ c=cnt[P[p],S[q]]+0; printf " %s=%d", S[q], c; e+=c }
    printf " exports=%d\n", e }
  printf "TOTAL done=%d partial=%d deferred=%d todo=%d exports=%d\n", tot["done"],tot["partial"],tot["deferred"],tot["todo"],rows
  printf "TIER1 done=%d partial=%d deferred=%d todo=%d\n", t1["done"],t1["partial"],t1["deferred"],t1["todo"]
}' PORT-MATRIX.md > /tmp/opencode/matrix-recount.$$.txt
cat /tmp/opencode/matrix-recount.$$.txt

# --- compare against the Current header table -------------------------------
fail=0
act=$(cat /tmp/opencode/matrix-recount.$$.txt)
hdrtot=$(awk '/^### Current/{f=1;next} f&&/total/{print;exit}' PORT-MATRIX.md)
hdrcore=$(awk '/^### Current/{f=1;next} f&&/@opentui\/core/{print;exit}' PORT-MATRIX.md)
hdrsolid=$(awk '/^### Current/{f=1;next} f&&/@opentui\/solid/{print;exit}' PORT-MATRIX.md)
hdrkm=$(awk '/^### Current/{f=1;next} f&&/keymap/{print;exit}' PORT-MATRIX.md)
hdr1=$(awk '/^### Current/{f=1;next} f&&/tier-1/{print;exit}' PORT-MATRIX.md)
# header cells: | name | exports | done | partial | deferred | todo |
cmprow(){ # cmprow <label> <header-line> <recount-line "pkg k=v ...">
  IFS='|' read -r _ _ hexp hdone hpart hdef htodo _ <<<"$2"
  for kv in $3; do case "$kv" in *=*) k=${kv%%=*}; v=${kv##*=};; *) continue;; esac
    case "$k" in exports) hv=${hexp:-};; done) hv=${hdone:-};; partial) hv=${hpart:-};; deferred) hv=${hdef:-};; todo) hv=${htodo:-};; *) continue;; esac
    hv=${hv//[*]/}; hv=$(echo "$hv"|cut -d'(' -f1|tr -d ' %')
    if [ "$hv" != "$v" ]; then echo "DRIFT $1.$k: header=$hv recount=$v"; fail=1; fi
  done
}
cmprow core   "$hdrcore"  "$(echo "$act"|awk '$1=="core"{print $2" "$3" "$4" "$5" " $6}')"
cmprow solid  "$hdrsolid" "$(echo "$act"|awk '$1=="solid"{print $2" "$3" "$4" "$5" "$6}')"
cmprow keymap "$hdrkm"    "$(echo "$act"|awk '$1=="keymap"{print $2" "$3" "$4" "$5" "$6}')"
cmprow total  "$hdrtot"   "$(echo "$act"|awk '$1=="TOTAL"{print $2" "$3" "$4" "$5}')"
cmprow tier1  "$hdr1"     "$(echo "$act"|awk '$1=="TIER1"{print $2" "$3" "$4" "$5}')"
rm -f /tmp/opencode/matrix-recount.$$.txt
[ $fail -eq 0 ] && echo "matrix header matches the per-symbol rows"
exit $fail
