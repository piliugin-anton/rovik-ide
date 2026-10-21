# Rovik IDE starts bash with this file (--init-file) in place of ~/.bashrc
# where readline, bash's line editor, can't redraw an input line taller than
# the terminal: bash 4.4 to 5.2 (readline before 8.3). Holding Backspace
# through a long paste blanked the row the caret moved up to. A line kept on
# one row that scrolls sideways (horizontal-scroll-mode) is never taller,
# but doesn't wrap either. So here a line wraps, and goes on one row only
# while it's too tall:
#
#   ESC [ 9001 ~  one row: the terminal sends it before a paste that may not
#                 fit, and before it shrinks with a line that may not fit;
#   ESC [ 9003 ~  one row if the line is too tall, wrapping if it fits: sent
#                 after such a paste, after deleting, and after a resize.
#
# Bash tells the terminal how its line is (OSC 7777, `__rovik_say`), and the
# terminal sends those keys only at a prompt this file set up, after keys
# that keep bash editing the line: never in a search (Ctrl+R), a
# completion's question, or a program a key runs (src/lib/bashLine.ts).

# Defined before the user's files run: an alias they define can't change
# what these do.

# How the line is, told to the terminal: "prompt;emacs" (or ";vi", the
# editing mode) at a prompt, "one-row;N" with the line N rows too tall
# (none or fewer: it fits), "wrap". On stderr, as bash's prompt and
# readline's output are.
__rovik_say() {
  builtin printf '\e]7777;rovik;%s\a' "$1" >&2
}

# The most columns $1, a line, takes, into __rovik_n: a column for each
# character, two when it takes three bytes or more in UTF-8 (CJK, emoji),
# four for a byte past ASCII in another encoding (\ooo), eight for a tab,
# two for another control character (^X). Never fewer than readline draws.
# Counted by deleting what's counted: bash takes long to delete many.
__rovik_columns() {
  local s=$1 t
  __rovik_n=${#s}
  local LC_ALL=C
  if [[ $s == *[$'\x80'-$'\xff']* ]]; then
    if [[ -n $__rovik_utf8 ]]; then
      t=${s//[$'\xe0'-$'\xf7']/}
      __rovik_n=$((__rovik_n + ${#s} - ${#t}))
    else
      t=${s//[$'\x80'-$'\xff']/}
      __rovik_n=$((__rovik_n + 3 * (${#s} - ${#t})))
    fi
  fi
  if [[ $s == *[$'\001'-$'\037'$'\177']* ]]; then
    t=${s//$'\t'/}
    __rovik_n=$((__rovik_n + 7 * (${#s} - ${#t})))
    t=${s//[$'\001'-$'\010'$'\013'-$'\037'$'\177']/}
    __rovik_n=$((__rovik_n + ${#s} - ${#t}))
  fi
}

# How many rows too tall the line being edited is, into __rovik_excess: none
# or fewer when it fits with a row to spare. Its rows, wrapped: from the
# prompt's last line, a row per COLUMNS columns of each of its lines, and
# one more.
__rovik_measure() {
  local u=$'\xc3\xa9' text=${READLINE_LINE-} cols=${COLUMNS:-80} limit=$((${LINES:-24} - 1))
  local prompt= plain= line t chars breaks width rows
  __rovik_utf8=
  if ((${#u} == 1)); then
    __rovik_utf8=1
  fi
  if ((cols < 1)); then
    cols=80
  fi
  # The prompt's last line, without what PS1 marks invisible (\[ \]). Once a
  # prompt.
  if [[ -z $__rovik_pw ]]; then
    if [[ -n ${PS1-} ]]; then
      prompt=${PS1@P}
    fi
    while [[ $prompt == *$'\001'*$'\002'* ]]; do
      plain+=${prompt%%$'\001'*}
      prompt=${prompt#*$'\002'}
    done
    plain+=$prompt
    __rovik_columns "${plain##*$'\n'}"
    __rovik_pw=$__rovik_n
  fi
  # Its characters alone say a long line's too tall: the most a row takes
  # is COLUMNS of them, or a line break and fewer.
  chars=${#text}
  rows=$(((__rovik_pw + chars + cols) / cols))
  if ((rows > limit)); then
    __rovik_excess=$((rows - limit))
    return
  fi
  # A short one's line breaks say it, a row each; then its lines do.
  t=${text//$'\n'/}
  breaks=$((chars - ${#t}))
  if ((breaks >= limit)); then
    __rovik_excess=$((breaks + 1 - limit))
    return
  fi
  rows=0
  width=$__rovik_pw
  while :; do
    line=${text%%$'\n'*}
    __rovik_columns "$line"
    rows=$((rows + (width + __rovik_n) / cols + 1))
    width=0
    if [[ $text != *$'\n'* ]]; then
      break
    fi
    text=${text#*$'\n'}
  done
  __rovik_excess=$((rows - limit))
}

# The line on one row ("one-row") or wrapping ("wrap"), and the terminal
# told. Bash draws it again once the key's command is done.
__rovik_show() {
  if [[ $1 == one-row ]]; then
    if [[ -z $__rovik_one_row ]]; then
      builtin bind 'set horizontal-scroll-mode on'
      __rovik_one_row=1
    fi
    __rovik_say "one-row;$__rovik_excess"
  else
    if [[ -n $__rovik_one_row ]]; then
      builtin bind 'set horizontal-scroll-mode off'
      __rovik_one_row=
    fi
    __rovik_say wrap
  fi
}

# ESC [ 9001 ~
__rovik_one_row() {
  __rovik_measure
  __rovik_show one-row
}

# ESC [ 9003 ~
__rovik_check() {
  __rovik_measure
  if ((__rovik_excess > 0)); then
    __rovik_show one-row
  else
    __rovik_show wrap
  fi
}

# Before each prompt: a new line wraps, and the terminal knows it's at a
# prompt, and in which editing mode.
__rovik_prompt() {
  if [[ -n $__rovik_one_row ]]; then
    builtin bind 'set horizontal-scroll-mode off'
    __rovik_one_row=
  fi
  __rovik_pw=
  if builtin shopt -oq vi; then
    __rovik_say 'prompt;vi'
  else
    __rovik_say 'prompt;emacs'
  fi
}

# Only where it can work, and the user hasn't chosen otherwise.
__rovik_setup() {
  local version=$((BASH_VERSINFO[0] * 100 + BASH_VERSINFO[1])) settings map
  # A key's command reads the line from 4.0 on, the prompt as shown from
  # 4.4 on; from 5.3 on, readline draws a tall line right.
  if ((version < 404 || version >= 503)); then
    return
  fi
  # Editing with readline: not --noediting, nor another line editor (ble.sh).
  if [[ $- != *i* || -n ${BLE_VERSION-} ]]; then
    return
  fi
  if ! builtin shopt -oq emacs && ! builtin shopt -oq vi; then
    return
  fi
  # Lines on one row: theirs, already. The terminal tells a paste by its
  # brackets.
  settings=$(builtin bind -v 2>/dev/null)
  if [[ $settings != *'horizontal-scroll-mode off'* || $settings != *'enable-bracketed-paste on'* ]]; then
    return
  fi
  __rovik_bash=1 __rovik_one_row= __rovik_pw= __rovik_utf8= __rovik_n=0 __rovik_excess=0
  for map in emacs vi-insert vi-command; do
    builtin bind -m "$map" -x '"\e[9001~": __rovik_one_row'
    builtin bind -m "$map" -x '"\e[9003~": __rovik_check'
  done
  # Run by this shell alone: one started in it may get PROMPT_COMMAND, if
  # it's exported, but none of this. Among bash-preexec's hooks where it's
  # loaded: what runs after its own in PROMPT_COMMAND counts as a command.
  if [[ -n ${bash_preexec_imported-}${__bp_imported-} ]]; then
    precmd_functions+=(__rovik_prompt)
  elif ((version >= 501)) && [[ -n ${PROMPT_COMMAND+set} && ${PROMPT_COMMAND@a} == *a* ]]; then
    PROMPT_COMMAND+=('${__rovik_bash+__rovik_prompt}')
  else
    PROMPT_COMMAND=${PROMPT_COMMAND:+$PROMPT_COMMAND$'\n'}'${__rovik_bash+__rovik_prompt}'
  fi
}

# A login shell's files, as `bash -l` reads them: a shell started with
# --init-file is no login shell, and reads no others.
if [ -f /etc/profile ] && [ -r /etc/profile ]; then
  . /etc/profile
fi
if [ -f ~/.bash_profile ] && [ -r ~/.bash_profile ]; then
  . ~/.bash_profile
elif [ -f ~/.bash_login ] && [ -r ~/.bash_login ]; then
  . ~/.bash_login
elif [ -f ~/.profile ] && [ -r ~/.profile ]; then
  . ~/.profile
fi

__rovik_setup
builtin unset -f __rovik_setup
