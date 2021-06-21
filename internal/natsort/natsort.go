package natsort

import (
	"regexp"
	"strconv"
	"strings"
)

// A parsed string always starts with a string component, even if empty.
// The order of a string is string[0], float[0], string [1], float[1] until one runs out.
// The float array will be either as long as the string array or one shorter.
type parsedString struct {
	stringSegments []string
	// The impetus for this was specifically chapter numbers (could be 32.5) so parse floats.
	// Otherwise "16.5:" will sort before "16:"
	floatSegments []float64
}

func compare(a, b parsedString) bool {
	i := 0
	for i = range a.stringSegments {
		if i == len(b.stringSegments) {
			// a is longer, a is larger
			return false
		}

		if a.stringSegments[i] != b.stringSegments[i] {
			return a.stringSegments[i] < b.stringSegments[i]
		}

		if i == len(a.floatSegments) {
			if i == len(b.floatSegments) {
				return false
			}
			// a is shorter, a is smaller
			return true
		}
		if i == len(b.floatSegments) {
			// a is longer
			return false
		}
		if a.floatSegments[i] != b.floatSegments[i] {
			return a.floatSegments[i] < b.floatSegments[i]
		}
	}

	// If b still has remaining components it's larger, otherwise they're equal.
	return len(b.stringSegments) > i+1
}

// NaturalSorter is a container used for one run of natural sorting.
// It memoizes the split strings for greater performance.
type NaturalSorter struct {
	parsedStrings map[string]parsedString
}

// NewNaturalSorter returns a freshly initialized NaturalSorter.
func NewNaturalSorter() NaturalSorter {
	return NaturalSorter{
		parsedStrings: make(map[string]parsedString),
	}
}

// We're not particularly interested in negative floats.
var floatRegex = regexp.MustCompile(`(\D*)(\d+(\.\d+)?)`)

func parseString(s string) parsedString {
	ss := []string{}
	fs := []float64{}
	s = strings.ToLower(s)

	matches := floatRegex.FindAllStringSubmatch(s, -1)
	if matches != nil {
		c := 0
		for _, m := range matches {
			ss = append(ss, m[1])
			f, _ := strconv.ParseFloat(m[2], 64)
			fs = append(fs, f)
			c += len(m[0])
		}
		s = s[c:]
	}
	ss = append(ss, s)

	return parsedString{
		stringSegments: ss,
		floatSegments:  fs,
	}
}

// Compare returns true if the first string is less than the second.
func (n NaturalSorter) Compare(a, b string) bool {
	ap, ok := n.parsedStrings[a]
	if !ok {
		ap = parseString(a)
		n.parsedStrings[a] = ap
	}
	bp, ok := n.parsedStrings[b]
	if !ok {
		bp = parseString(b)
		n.parsedStrings[b] = bp
	}
	return compare(ap, bp)
}
