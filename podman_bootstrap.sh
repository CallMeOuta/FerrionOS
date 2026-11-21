#!/usr/bin/env bash


set -e

banner()
{
  echo "|------------------------------------------|"
  echo "|--- Welcome to the FerrionOS bootstrap ---|"
  echo "|-------- for building with Podman --------|"
  echo "|------------------------------------------|"
}

install_bsd_pkg()
{
    PKG_MANAGER=$1
    PKG_NAME=$2
    BIN_NAME=$3
    if [ -z "$BIN_NAME" ]; then
        BIN_NAME=$PKG_NAME
    fi

    BIN_LOCATION=$(which $BIN_NAME || true)
    if [ -z "$BIN_LOCATION" ]; then
        echo "$PKG_MANAGER install $PKG_NAME"
        $PKG_MANAGER install "$PKG_NAME"
    else
        echo "$BIN_NAME already exists at $BIN_LOCATION, no need to install $PKG_NAME..."
    fi
}

install_macports_pkg()
{
    install_bsd_pkg "sudo port" "$1" "$2"
}

install_brew_pkg()
{
    install_bsd_pkg "brew" $@
}

install_brew_cask_pkg()
{
    install_bsd_pkg "brew cask" $@
}

install_freebsd_pkg()
{
    install_bsd_pkg "sudo pkg" $@
}

osx()
{
    echo "Detected macOS!"

    if [ ! -z "$(which brew)" ]; then
        osx_homebrew $@
    elif [ ! -z "$(which port)" ]; then
        osx_macports $@
    else
        echo "Please install either Homebrew or MacPorts, if you wish to use this script"
        echo "Re-run this script once you installed one of those package managers"
        echo "Will not install, now exiting..."
        exit 1
    fi
}

osx_macports()
{
    echo "MacPorts detected! Now updating..."
    sudo port -v selfupdate

    echo "Installing missing packages..."

    install_macports_pkg "git"
    install_macports_pkg "gmake"
    install_macports_pkg "curl"
    install_macports_pkg "podman"
    install_macports_pkg "gdb +multiarch"

    if [ "$1" == "qemu" ]; then
        install_macports_pkg "qemu" "qemu-system-x86_64"
    elif [ "$1" == "virtualbox" ]; then
        install_macports_pkg "virtualbox"
    else
        echo "Unknown emulator: $1"
        exit 1
    fi
}

osx_homebrew()
{
    echo "Homebrew detected! Now updating..."
    brew update

    echo "Installing missing packages..."

    install_brew_pkg "git"
    install_brew_pkg "make"
    install_brew_pkg "curl"
    install_brew_pkg "podman"
    install_brew_pkg "gdb"

    if [ "$1" == "qemu" ]; then
        install_brew_pkg "qemu" "qemu-system-x86_64"
    elif [ "$1" == "virtualbox" ]; then
        install_brew_pkg "virtualbox"
    else
        echo "Unknown emulator: $1"
        exit 1
    fi
}

freebsd()
{
    set -x
    echo "FreeBSD detected!"
    echo "Installing missing packages..."

    install_freebsd_pkg "git"
    install_freebsd_pkg "gmake"
    install_freebsd_pkg "curl"
    install_freebsd_pkg "podman"
    install_freebsd_pkg "gdb"

    if [ "$1" == "qemu" ]; then
        install_freebsd_pkg "qemu" "qemu-system-x86_64"
    elif [ "$1" == "virtualbox" ]; then
        install_freebsd_pkg "virtualbox"
    else
        echo "Unknown emulator: $1"
        exit 1
    fi

    set +x
}

archLinux()
{
    echo "Detected Arch Linux"
    packages="git make curl fuse3 fuse-overlayfs slirp4netns podman gdb openssl python"
    if [ "$1" == "qemu" ]; then
        packages="$packages qemu-desktop qemu-system-arm qemu-system-riscv"
    elif [ "$1" == "virtualbox" ]; then
        packages="$packages virtualbox"
    else
        echo "Unknown emulator: $1"
        exit 1
    fi

    echo "Installing packages $packages..."
    sudo pacman -S --needed $packages
}

ubuntu()
{
    echo "Detected Ubuntu/Debian"
    echo "Updating system..."
    sudo "$2" update
    echo "Installing required packages..."
    sudo "$2" install \
        podman curl git make pkg-config fuse3 libfuse3-dev fuse-overlayfs slirp4netns gdb-multiarch openssl python3
    if [ "$1" == "qemu" ]; then
        if [ -z "$(which qemu-system-x86_64)" ]; then
            echo "Installing QEMU..."
            if apt-cache show qemu-system-riscv > /dev/null 2>&1
            then riscv="qemu-system-riscv"
            else riscv="qemu-system-riscv64"
            fi
            sudo "$2" install qemu-system-x86 qemu-kvm \
                qemu-system-arm qemu-efi-aarch64 $riscv
        else
            echo "QEMU already installed!"
        fi
    elif [ "$1" == "virtualbox" ]; then
        if [ -z "$(which virtualbox)" ]; then
            if grep '^ID=debian$' /etc/os-release > /dev/null; then
                echo "Virtualbox is not in the official debian packages"
                echo "To install virtualbox on debian, see https://wiki.debian.org/VirtualBox"
                echo "Please install VirtualBox and re-run this script,"
                echo "or run with -e qemu"
                exit 1
            else
                echo "Installing VirtualBox..."
                sudo "$2" install virtualbox
            fi
        else
            echo "VirtualBox already installed!"
        fi
    else
        echo "Unknown emulator: $1"
        exit 1
    fi
}

fedora()
{
    echo "Detected Fedora"
    if [ -z "$(which git)" ]; then
        echo "Installing git..."
        sudo dnf install git-all
    fi

    if [ "$1" == "qemu" ]; then
        if [ -z "$(which qemu-system-x86_64)" ]; then
            echo "Installing QEMU..."
            sudo dnf install qemu-system-x86 qemu-system-arm \
            qemu-system-riscv qemu-kvm edk2-aarch64
        else
            echo "QEMU already installed!"
        fi
    elif [ "$1" == "virtualbox" ]; then
        if [ -z "$(which virtualbox)" ]; then
            echo "Please install VirtualBox and re-run this script,"
            echo "or run with -e qemu"
            exit 1
        else
            echo "VirtualBox already installed!"
        fi
    else
        echo "Unknown emulator: $1"
        exit 1
    fi

    PKGS=$(for pkg in podman curl make fuse3 fuse3-devel fuse-overlayfs slirp4netns gdb openssl python3; do rpm -q $pkg > /dev/null || echo $pkg; done)
    COUNT=$(echo $PKGS | wc -w)
    if [ $COUNT -ne 0 ]; then
        echo "Installing necessary build tools..."
        sudo dnf install $PKGS
    fi
}

suse()
{
    echo "Detected SUSE Linux"

    packages=(
        "git"
        "curl"
        "make"
        "fuse"
        "fuse-overlayfs"
        "slirp4netns"
        "podman"
        "gdb-multiarch"
        "openssl"
        "python3"
    )

    if [ -z "$(which git)" ]; then
        echo "Will install git ..."
        packages+=(git)
    fi

    if [ "$1" == "qemu" ]; then
        if [ -z "$(which qemu-system-x86_64)" ]; then
            echo "Will install QEMU..."
            packages+=(qemu-x86 qemu-kvm)
        else
            echo "QEMU already installed!"
        fi
    elif [ "$1" == "virtualbox" ]; then
        if [ -z "$(which virtualbox)" ]; then
            echo "Please install VirtualBox and re-run this script,"
            echo "or run with -e qemu"
            exit 1
        else
            echo "VirtualBox already installed!"
        fi
    else
        echo "Unknown emulator: $1"
        exit 1
    fi

    echo "Installing necessary build tools..."

    for p in ${packages[@]}; do
        if rpm -q "${p}" > /dev/null ; then
            echo "${p} is already installed"
        else
            if out="$(zypper --non-interactive install --no-confirm --dry-run --force-resolution ${p}  2>&1)"  ; then
                echo "${p} can be installed"
            else
                echo "no"
                echo ""
                echo "Zypper output:"
                echo ""
                echo "${out}"
                echo ""
                echo "Could not find how to install '${p}', try running:"
                echo ""
                echo "     zypper install ${p}"
                echo ""
                exit 1
            fi
        fi
    done

    zypper install ${packages[@]}
}

gentoo()
{
    echo "Detected Gentoo Linux"
    if [ -z "$(which git)" ]; then
        echo "Installing git..."
        sudo emerge dev-vcs/git
    fi
    if [ -z "$(which fusermount 2>/dev/null)" ] && [ -z "$(which fusermount3 2>/dev/null)" ]; then
        echo "Installing fuse..."
        sudo emerge sys-fs/fuse
    fi

    if [ "$1" == "qemu" ]; then
        if [ -z "$(which qemu-system-x86_64)" ]; then
            echo "Please install QEMU and re-run this script"
            echo "Step1. Add QEMU_SOFTMMU_TARGETS=\"x86_64\" to /etc/portage/make.conf"
            echo "Step2. Execute \"sudo emerge app-emulation/qemu\""
            exit 1
        else
            echo "QEMU already installed!"
        fi
    elif [ "$1" == "virtualbox" ]; then
        if [ -z "$(which virtualbox)" ]; then
            echo "Please install VirtualBox and re-run this script,"
            echo "or run with -e qemu"
            exit 1
        else
            echo "VirtualBox already installed!"
        fi
    else
        echo "Unknown emulator: $1"
        exit 1
    fi

    if [ -z "$(which cmake)" ]; then
        echo "Installing cmake..."
        sudo emerge dev-util/cmake
    fi
    if [ -z "$(which podman)" ]; then
        echo "Please install Podman, https://wiki.gentoo.org/wiki/Podman"
        exit 1
    fi
}

solus()
{
    echo "Detected Solus"

    if [ "$1" == "qemu" ]; then
        if [ -z "$(which qemu-system-x86_64)" ]; then
            sudo eopkg it qemu
        else
            echo "QEMU already installed!"
        fi
    elif [ "$1" == "virtualbox" ]; then
        if [ -z "$(which virtualbox)" ]; then
            echo "Please install VirtualBox and re-run this script,"
            echo "or run with -e qemu"
            exit 1
        else
            echo "VirtualBox already installed!"
        fi
    else
        echo "Unknown emulator: $1"
        exit 1
    fi

    echo "Installing necessary build tools..."
    sudo eopkg it fuse-devel git make fuse2-devel rsync openssl python3
    if [ -z "$(which podman)" ]; then
        echo "Please install Podman"
        exit 1
    fi
}

usage()
{
    echo "----------------------------"
    echo "|FerrionOS bootstrap script|"
    echo "----------------------------"
    echo "Usage: ./podman_bootstrap.sh"
    echo "OPTIONS:"
    echo
    echo "   -h,--help      Show this prompt"
    echo "   -u [branch]    Update git repo and update rust"
    echo "                  If blank defaults to master"
    echo "   -e [emulator]  Install specific emulator, virtualbox or qemu"
    echo "   -p [package    Choose an Ubuntu package manager, apt-fast or"
    echo "       manager]   aptitude"
    echo "   -d             Only install the dependencies, skip boot step"
    echo "EXAMPLES:"
    echo
    echo "./podman_bootstrap.sh -e qemu"
    exit
}

rustInstall()
{
    noninteractive=$1
    if [ -e /usr/local/lib/rustlib/uninstall.sh ] ; then
        echo "It appears that multirust is installed on your system."
        echo "This tool has been deprecated by the maintainer, and will cause issues."
        echo "This script can remove multirust from your system if you wish."
        printf "Uninstall multirust (y/N):"
        read multirust
        if echo "$multirust" | grep -iq "^y" ;then
            sudo /usr/local/lib/rustlib/uninstall.sh
        else
            echo "Please manually uninstall multirust and any other versions of rust, then re-run bootstrap."
            exit 1
        fi
    fi
    if [ -z "$(which rustup)" ]; then
        rustup_options="--default-toolchain stable"
        echo "You do not have rustup installed."
        if [ "$noninteractive" = true ]; then
            rustup="y"
            rustup_options+=" -y"
        else
            echo "We HIGHLY recommend using rustup."
            echo "Would you like to install it now?"
            echo "*WARNING* this involves a 'curl | sh' style command"
            printf "(y/N): "
            read rustup
        fi
        if echo "$rustup" | grep -iq "^y" ;then
            curl https://sh.rustup.rs -sSf | sh -s -- $rustup_options
            echo "export PATH=\"\$HOME/.cargo/bin:\$PATH\"" >> ~/.bashrc
            source ~/.cargo/env
        else
            echo "Rustup will not be installed!"
        fi
    fi
    if [ -z "$(which rustc)" ]; then
        echo "Rust is not installed"
        echo "Please either run the script again, accepting rustup install"
        echo "or install rustc stable manually (not recommended) via:"
        echo "\#curl -sSf https://static.rust-lang.org/rustup.sh | sh -s -- --channel=stable"
        exit 1
    else
        echo "Your Rust install looks good!"
    fi
}

boot()
{
    echo "Cloning gitlab repo..."
    git clone https://gitlab.redox-os.org/redox-os/redox.git --origin upstream
    echo "Creating .config with PODMAN_BUILD=1"
    echo 'PODMAN_BUILD?=1' > redox/.config
    if [[ "$(uname -m)" == "arm64" ]]; then
        echo "Appending .config with ARCH=aarch64"
        echo 'ARCH=aarch64' >> redox/.config
    fi
    echo "Cleaning up..."
    rm podman_bootstrap.sh
    echo
    echo "---------------------------------------"
    echo "Well it looks like you are ready to go!"
    echo "---------------------------------------"
    echo "The file redox/.config was created with PODMAN_BUILD=1."
    echo "If you need a much quicker installation, run: "
    echo "  echo REPO_BINARY=1 >> redox/.config"
    echo
    echo "Run the following commands to build FerrionOS using Podman:"
    echo
    echo "cd redox"
    MAKE="make"
    if [[ "$(uname)" == "FreeBSD" ]]; then
        MAKE="gmake"
    fi
    echo "$MAKE all"
    echo "$MAKE $emulator"
    echo
    echo "      Good luck!"

    exit
}

if [ "$1" == "-h" ] || [ "$1" == "--help" ]; then
    usage
elif [ "$1" == "-u" ]; then
    git pull upstream master
    exit
fi

emulator="qemu"
defpackman="apt-get"
dependenciesonly=false
update=false
while getopts ":e:p:udhs" opt
do
    case "$opt" in
        e) emulator="$OPTARG";;
        p) defpackman="$OPTARG";;
        d) dependenciesonly=true;;
        u) update=true;;
        h) usage;;
        \?) echo "I don't know what to do with that option, try -h for help"; exit 1;;
    esac
done

banner

rustInstall "$noninteractive"

if [ "$update" == "true" ]; then
    git pull upstream master
    exit
fi

if [ "Darwin" == "$(uname -s)" ]; then
    osx "$emulator"
else

    if hash 2>/dev/null zypper; then
        suse "$emulator"
    elif hash 2>/dev/null dnf; then
        fedora "$emulator"
    elif hash 2>/dev/null apt-get; then
        ubuntu "$emulator" "$defpackman"
    elif hash 2>/dev/null emerge; then
        gentoo "$emulator"
    elif hash 2>/dev/null eopkg; then
        solus "$emulator"
    elif hash 2>/dev/null pacman; then
        archLinux "$emulator"
    elif hash 2>/dev/null pkg; then
        freebsd "$emulator"
    else
        printf "\e[31;1mFatal error: \e[0;31mUnsupported platform, please open an issue\e[0m\n"
    fi
fi

if [ "$dependenciesonly" = false ]; then
    boot
fi

echo "FerrionOS bootstrap complete!"
