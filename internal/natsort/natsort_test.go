package natsort

import "testing"

func verifyGte(t *testing.T, a, b string) {
	n := NewNaturalSorter()
	if n.Compare(a, b) {
		t.Fatalf("Expected [%s] >= [%s] but got the opposite", a, b)
	}
}

func verifyLt(t *testing.T, a, b string) {
	n := NewNaturalSorter()
	if !n.Compare(a, b) {
		t.Fatalf("Expected [%s] >= [%s] but got the opposite", a, b)
	}
}

func Test_SortNoNumbers(t *testing.T) {
	verifyGte(t, "a", "a")
	verifyLt(t, "a", "b")
	verifyLt(t, "abc", "abcd")
	verifyLt(t, "abc", "abd")
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
	verifyLt(t, "16:", "16.5:")
}
