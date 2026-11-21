old=$(stty -g)
stty -echo
stty | grep -q -- '-echo' && echo "echo off"
new=$(stty -g)
stty "$old"
[ "$(stty -g)" = "$old" ] && [ "$new" != "$old" ] && echo "termios ok"
