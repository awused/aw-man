// -build windows

package config

import "syscall"

// SysProcAttr does nothing on linux.
var SysProcAttr = &syscall.SysProcAttr{}
