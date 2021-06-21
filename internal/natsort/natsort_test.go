package natsort

import (
	"math/rand"
	"sort"
	"testing"
)

func verifyGte(t *testing.T, a, b string) {
	n := NewNaturalSorter()
	if n.Compare(a, b) {
		t.Fatalf("Expected [%s] >= [%s] but got the opposite", a, b)
	}
}

func verifyEq(t *testing.T, a, b string) {
	n := NewNaturalSorter()
	if n.Compare(a, b) || n.Compare(b, a) {
		t.Fatalf("Expected [%s] == [%s] but got the opposite", a, b)
	}
}
func verifyLt(t *testing.T, a, b string) {
	n := NewNaturalSorter()
	if !n.Compare(a, b) {
		t.Fatalf("Expected [%s] < [%s] but got the opposite", a, b)
	}
}

func Test_SortNoNumbers(t *testing.T) {
	verifyGte(t, "a", "a")
	verifyLt(t, "a", "b")
	verifyLt(t, "abc", "abcd")
	verifyLt(t, "abc", "abd")
	verifyLt(t, "ABC", "abd")
	verifyLt(t, "aBC", "Abd")
	verifyLt(t, "aBc", "AbD")
	verifyGte(t, "ABC", "abc")
	verifyGte(t, "abc", "ABC")
	verifyLt(t, "", "ABC")
}

func Test_NumbersOnly(t *testing.T) {
	verifyGte(t, "17", "17")
	verifyLt(t, "16", "16.5")
	verifyLt(t, "4", "5")
	verifyGte(t, "17", "16.7")
}

func Test_Combined(t *testing.T) {
	verifyGte(t, "abc 10 abc 20", "abc 10 abc 20")
	verifyLt(t, "abc 10 abc 16", "abc 10 abc 16.5")
	verifyGte(t, "abc 10 abd 16", "abc 10 abc 16.5")
}

func Test_IntFailCase(t *testing.T) {
	// This case fails when integer based tokenization is used.
	verifyLt(t, "16:", "16.5:")
}

func Test_Unicode(t *testing.T) {
	verifyEq(t, "K", "K") // Kelvin sign
	verifyLt(t, "あ", "い")
	verifyLt(t, "あ", "雨")
	// Would require Mecab to sort these properly
	// verifyLt(t, "雨", "い")
	// verifyLt(t, "い", "ア")
	// verifyLt(t, "あ", "ア")
}

func Test_ExampleFiles(t *testing.T) {
	// From http://davekoelle.com/alphanum.html plus some additions
	unsorted := []string{
		"z1.doc",
		"z10.doc",
		"z100.5.doc",
		"z100.eoc",
		"z101.doc",
		"z102.doc",
		"z11.doc",
		"z12.doc",
		"z13.doc",
		"z14.doc",
		"z15.doc",
		"z16.doc",
		"z17.doc",
		"z18.doc",
		"z19.DOC",
		"z2.doc",
		"Z20.doc",
		"a3.doc",
		"z4.doc",
		"z4.5.doc",
		"z4.3.doc",
		"z4.75.doc",
		"z4.7.doc",
		"Z5.doc",
		"B6.DOC",
		"z7.doc",
		"c8.doc",
		"z9.doc",
	}

	sorted := []string{
		"a3.doc",
		"B6.DOC",
		"c8.doc",
		"z1.doc",
		"z2.doc",
		"z4.doc",
		"z4.3.doc",
		"z4.5.doc",
		"z4.7.doc",
		"z4.75.doc",
		"Z5.doc",
		"z7.doc",
		"z9.doc",
		"z10.doc",
		"z11.doc",
		"z12.doc",
		"z13.doc",
		"z14.doc",
		"z15.doc",
		"z16.doc",
		"z17.doc",
		"z18.doc",
		"z19.DOC",
		"Z20.doc",
		"z100.eoc",
		"z100.5.doc",
		"z101.doc",
		"z102.doc",
	}

	ns := NewNaturalSorter()
	sort.Slice(unsorted, func(i, j int) bool {
		return ns.Compare(unsorted[i], unsorted[j])
	})

	for i, s := range unsorted {
		if sorted[i] != s {
			t.Fatalf("Incorrect sorted order, got:\n %s\n expected:\n %s\n", unsorted, sorted)
		}
	}
}

// Benchmarks

// Bias towards more numbers and periods
const characters = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456790123456789._-..."

func randomStrings(n int) []string {
	var ss []string
	for i := 0; i < n; i++ {
		b := make([]byte, rand.Intn(100))
		for j := range b {
			b[j] = characters[rand.Intn(len(characters))]
		}
		ss = append(ss, string(b))
	}

	return ss
}

func benchmarkSort(b *testing.B, n int) {
	for i := 0; i < b.N; i++ {
		b.StopTimer()
		ss := randomStrings(n)
		b.StartTimer()

		ns := NewNaturalSorter()
		sort.Slice(ss, func(i, j int) bool {
			return ns.Compare(ss[i], ss[j])
		})
	}
}

func Benchmark_Sort10(b *testing.B) {
	benchmarkSort(b, 10)
}
func Benchmark_Sort100(b *testing.B) {
	benchmarkSort(b, 100)
}
func Benchmark_Sort1000(b *testing.B) {
	benchmarkSort(b, 1000)
}
func Benchmark_Sort10000(b *testing.B) {
	benchmarkSort(b, 10000)
}
func Benchmark_Sort100000(b *testing.B) {
	benchmarkSort(b, 100000)
}
func Benchmark_Sort1000000(b *testing.B) {
	benchmarkSort(b, 1000000)
}
