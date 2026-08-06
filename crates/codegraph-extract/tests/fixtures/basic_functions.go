package main

import "os"

func realMain() int {
    return 0
}

func main() {
    os.Exit(realMain())
}