// +build windows

package config

import "syscall"

// SysProcAttr informs Windows not to show a console window for child processes.
var SysProcAttr = &syscall.SysProcAttr{HideWindow: true}
