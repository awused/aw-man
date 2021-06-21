package resources

import _ "embed"

// NotoSansRegular is the font used by aw-man.
// It doesn't support everything in one font but it's probably good enough for a manga reader.
//go:embed fonts/NotoSansCJKjp-Regular.otf
var NotoSansRegular []byte
