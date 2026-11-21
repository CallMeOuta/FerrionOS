#!/usr/bin/env bash


usage()
{
    echo "Usage: $0 -r recipe [ -e command_name ] [ -R ] [ -X | -A ] [[ -b backtracefile ] | [ addr1 ... ]]"
    echo
    echo "Print the backtrace contained in the backtracefile."
    echo "Symbols are taken from the executable for the given recipe."
    echo "If no backtracefile is given, decode the given addresses instead."
    echo "This command must be run in the build system's root directory."
    echo
    echo "-X for x86_64, -A for aarch64 (x86_64 is the default)."
    echo "To read from stdin, use '-b -'"
    echo "The name of the executable must match what Cargo believes it to be."
    echo "If the executalbe is named 'recipe_command', just use 'command' as the name."
    echo "The debug version of the executable is used if available."
    echo "The release version is used if no debug version exists."
    echo "-R to force the use of the 'release' version of the executable."
    echo "Make sure the executable is the one that produced the backtrace."
    exit 1
}

ARCH="x86_64"

while getopts ":b:e:r:hRXA" opt
do
    case "$opt" in
        X) ARCH="x86_64";;
        A) ARCH="aarch64";;
        b) INFILE="$OPTARG";;
        e) COMMAND="$OPTARG";;
        i) INST="$OPTARG";;
        r) RECIPE_NAME="$OPTARG";;
        R) RELEASE=true;;
	h) usage;;
	\?) echo "Unknown option -$OPTARG, try -h for help"; exit;;
        :) echo "-$OPTARG requires a value"; exit;;
	esac
done
shift $((OPTIND -1))

if [ -z "$RECIPE_NAME" ]
then
    usage
fi

if [ -z "$INFILE" -a $# = 0 ]
then
    usage
fi

RECIPE_DIR="$(target/release/repo find $RECIPE_NAME)"
if [ -z "$COMMAND" ]
then
    COMMAND="$RECIPE_NAME"
fi

EXECUTABLE="$RECIPE_DIR"/target/"$ARCH"-unknown-redox/build/target/"$ARCH"-unknown-redox/debug/"$COMMAND"

if [ ! -f "$EXECUTABLE" -o ! -z "$RELEASE" ]
then
    EXECUTABLE="$RECIPE_DIR"/target/"$ARCH"-unknown-redox/build/target/"$ARCH"-unknown-redox/release/"$COMMAND"
fi

if [ $# -ne 0 ]
then
    addr2line --demangle=rust --inlines --pretty-print --functions --exe="$EXECUTABLE" $@
else
    sed '/^\s*$/d; s/^.*0x\([0-9a-f]*\).*$/\1/g' "$INFILE" | addr2line --demangle=rust --inlines --pretty-print --functions --exe="$EXECUTABLE"
fi

