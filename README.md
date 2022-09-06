# WIP

This is a work in progress. Nothing here is stable and ready for use.
I made this project as a way to learn the Rust programming language.
There are many things I can (and would like) to improve.

*If you have any suggestion please let me know :)*

# Rlwrap
The main goals of this project are:
 - Transparently intercept user input/output in order to provide a fancy readline prompt.
 - Basic line editing functionality.
 - Make it easy to insert a prompt in your app by using this as a library.

# Implementation

This is project contains a library and a binary.

## The Rlwrap library

The library works by creating a pseudo-terminal (pty) and redirecting
the I/O of the current process to it. Then it makes two threads that will
use the original I/O, one that makes an input prompt, and another that
takes care of writing the output without messing up the prompt.

## The Rlwrap binary

The binary uses the library to execute any program and provides some
configuration options.
Example: `rlwrap nc google.com 80`

# TODO

- Change cursor position in line editor
- History
- Completions
