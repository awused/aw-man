package closing

import "sync"

// Ch is used to signal that the program is closing.
var Ch chan struct{} = make(chan struct{})
var closeSync sync.Once

// Once is used to start closing the program
func Once() {
	closeSync.Do(func() { close(Ch) })
}
