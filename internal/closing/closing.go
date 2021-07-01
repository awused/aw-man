package closing

import "sync"

// Ch is used to signal that the program is closing.
var Ch chan struct{} = make(chan struct{})
var closeSync sync.Once

// Close is used to start closing the program
func Close() {
	closeSync.Do(func() { close(Ch) })
}
