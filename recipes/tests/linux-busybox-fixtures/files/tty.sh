# Terminal attributes through busybox stty (TCGETS and TCSETS), run with a terminal on stdin:
# echo is turned off and seen off, then the saved attributes are put back and read back equal.
old=$(stty -g)
stty -echo
stty | grep -q -- '-echo' && echo "echo off"
new=$(stty -g)
stty "$old"
[ "$(stty -g)" = "$old" ] && [ "$new" != "$old" ] && echo "termios ok"
