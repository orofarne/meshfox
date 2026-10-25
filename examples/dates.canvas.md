<!-- meshfox:canvas -->
# Dates and times
<!-- meshfox:node id="root" -->

A small calculator that doubles as a tour of the `date` and `datetime`
variable types (see SPEC.md's "Date and datetime variables" and "Date
pickers"). Two forms set the values; the blocks under them are `autorun`, so
pressing **Apply** recalculates right away.

| Variable | Type | What it shows |
| --- | --- | --- |
| `START_DAY`, `END_DAY` | `date` | a calendar day, `YYYY-MM-DD` — the browser's date picker, or `Ctrl-O` in the terminal |
| `OPENS`, `CLOSES` | `datetime` | an instant **with its UTC offset**, `2026-10-25T09:00:00+03:00` |

Both can be typed by hand or picked from a calendar. Either way a block
only ever receives the canonical form, so it can be taken apart with plain
shell parameter expansion — no `date` command, which differs between macOS
and Linux. A value that is not a real day (`2026-02-30`) or a datetime
without an offset is rejected, in the form, on the command line
(`--set START_DAY=2026-02-30`) and in the cache.

<!-- meshfox:var name="START_DAY" type="date" prompt="From" default="2026-01-01" -->
<!-- meshfox:var name="END_DAY" type="date" prompt="To" default="2026-12-31" -->
<!-- meshfox:var name="OPENS" type="datetime" prompt="Opens" default="2026-10-25T09:00:00+03:00" -->
<!-- meshfox:var name="CLOSES" type="datetime" prompt="Closes" default="2026-10-25T18:30:00+01:00" -->

## Days between two dates
<!-- meshfox:node id="days" -->

Calendar days from `START_DAY` to `END_DAY` (the second day is not counted;
add one for an inclusive range). The two dates need not be in order.

```form name="days-form" send="Apply"
field var="START_DAY" label="From"
field var="END_DAY" label="To"
```

The day number is the proleptic Gregorian "days since 1970-01-01" count, so
leap years and month lengths come out right.

```bash name="days-between" autorun env="$START_DAY,$END_DAY"
set -euo pipefail

# Days since 1970-01-01 of a canonical YYYY-MM-DD.
days_since_epoch() {
  local y=$((10#${1%%-*})) rest=${1#*-}
  local m=$((10#${rest%%-*})) d=$((10#${rest#*-}))
  (( m <= 2 )) && y=$((y - 1))
  local era=$(( (y >= 0 ? y : y - 399) / 400 ))
  local yoe=$(( y - era * 400 ))
  local doy=$(( (153 * ((m + 9) % 12) + 2) / 5 + d - 1 ))
  local doe=$(( yoe * 365 + yoe / 4 - yoe / 100 + doy ))
  echo $(( era * 146097 + doe - 719468 ))
}

diff=$(( $(days_since_epoch "$END_DAY") - $(days_since_epoch "$START_DAY") ))
abs=${diff#-}
if (( diff >= 0 )); then
  echo "$START_DAY → $END_DAY: $diff calendar days"
else
  echo "$END_DAY is $abs calendar days before $START_DAY"
fi
echo "= $(( abs / 7 )) weeks and $(( abs % 7 )) days; $(( abs + 1 )) if both end days are counted"
```

## Hours between two moments
<!-- meshfox:node id="hours" -->

Elapsed time from `OPENS` to `CLOSES`. Each value carries its own offset, so
the two may be in different time zones — the defaults are `+03:00` and
`+01:00`, and the answer is still the real elapsed time. A field in the
terminal form accepts `2026-10-25 09:00` and attaches your local offset; the
web form has an offset box next to the date and time.

```form name="hours-form" send="Apply"
field var="OPENS" label="Opens"
field var="CLOSES" label="Closes"
```

```bash name="hours-between" autorun env="$OPENS,$CLOSES"
set -euo pipefail

days_since_epoch() {
  local y=$((10#${1%%-*})) rest=${1#*-}
  local m=$((10#${rest%%-*})) d=$((10#${rest#*-}))
  (( m <= 2 )) && y=$((y - 1))
  local era=$(( (y >= 0 ? y : y - 399) / 400 ))
  local yoe=$(( y - era * 400 ))
  local doy=$(( (153 * ((m + 9) % 12) + 2) / 5 + d - 1 ))
  local doe=$(( yoe * 365 + yoe / 4 - yoe / 100 + doy ))
  echo $(( era * 146097 + doe - 719468 ))
}

# Unix seconds of a canonical RFC3339 value (fractional seconds ignored).
unix_seconds() {
  local date=${1%%T*} rest=${1#*T}
  local clock=${rest:0:8} offset=0
  case $rest in
    *Z) ;;
    *) local tz=${rest: -6} sign=1
       [[ $tz == -* ]] && sign=-1
       offset=$(( sign * (10#${tz:1:2} * 3600 + 10#${tz:4:2} * 60) )) ;;
  esac
  echo $(( $(days_since_epoch "$date") * 86400 \
         + 10#${clock:0:2} * 3600 + 10#${clock:3:2} * 60 + 10#${clock:6:2} - offset ))
}

diff=$(( $(unix_seconds "$CLOSES") - $(unix_seconds "$OPENS") ))
abs=${diff#-}
span="$(( abs / 3600 )) h $(( abs % 3600 / 60 )) min"
if (( diff >= 0 )); then
  echo "$OPENS → $CLOSES: $span"
else
  echo "$CLOSES is $span before $OPENS"
fi
echo "= $abs seconds"
```
