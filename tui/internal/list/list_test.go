package list

import "testing"

// testItem is a simple Item implementation for tests.
type testItem struct {
	label      string
	selectable bool
}

func (i testItem) Label() string    { return i.label }
func (i testItem) Selectable() bool { return i.selectable }

func selectable(label string) testItem { return testItem{label: label, selectable: true} }
func header(label string) testItem     { return testItem{label: label, selectable: false} }

func TestNewSelectsFirstSelectableItem(t *testing.T) {
	l := New(header("Provider"), selectable("model-a"), selectable("model-b"))
	if l.Selected() != 1 {
		t.Fatalf("Selected() = %d, want 1", l.Selected())
	}
}

func TestNewWithNoSelectableItems(t *testing.T) {
	l := New(header("A"), header("B"))
	if l.Selected() != -1 {
		t.Fatalf("Selected() = %d, want -1", l.Selected())
	}
}

func TestSelectNextWrapsAround(t *testing.T) {
	l := New(selectable("a"), selectable("b"), selectable("c"))
	// Selected starts at 0 ("a").
	l.SelectNext() // -> 1 ("b")
	l.SelectNext() // -> 2 ("c")
	if !l.SelectNext() {
		t.Fatal("SelectNext should return true on wrap")
	}
	if l.Selected() != 0 {
		t.Fatalf("after wrap, Selected() = %d, want 0", l.Selected())
	}
}

func TestSelectPrevWrapsAround(t *testing.T) {
	l := New(selectable("a"), selectable("b"), selectable("c"))
	// Selected starts at 0 ("a").
	if !l.SelectPrev() {
		t.Fatal("SelectPrev should return true on wrap")
	}
	if l.Selected() != 2 {
		t.Fatalf("after wrap, Selected() = %d, want 2", l.Selected())
	}
}

func TestSelectNextSkipsHeaders(t *testing.T) {
	l := New(selectable("a"), header("H"), selectable("b"))
	l.SelectNext() // -> skip header -> 2 ("b")
	if l.Selected() != 2 {
		t.Fatalf("Selected() = %d, want 2", l.Selected())
	}
}

func TestSelectPrevSkipsHeaders(t *testing.T) {
	l := New(selectable("a"), header("H"), selectable("b"))
	l.SetSelected(2) // start at "b"
	l.SelectPrev()   // skip header -> 0 ("a")
	if l.Selected() != 0 {
		t.Fatalf("Selected() = %d, want 0", l.Selected())
	}
}

func TestScrollToSelectedAdjustsOffset(t *testing.T) {
	items := make([]Item, 20)
	for i := range items {
		items[i] = selectable(string(rune('a' + i)))
	}
	l := New(items...)
	l.SetSize(40, 5)
	l.SetSelected(15)
	l.ScrollToSelected()
	if l.Offset() < 11 {
		t.Fatalf("Offset() = %d, want >= 11", l.Offset())
	}
}

func TestSetItemsResetsSelection(t *testing.T) {
	l := New(selectable("a"), selectable("b"))
	l.SetSelected(1)
	l.SetItems(header("H"), selectable("x"), selectable("y"))
	if l.Selected() != 1 {
		t.Fatalf("after SetItems, Selected() = %d, want 1", l.Selected())
	}
}

func TestIsSelectedFirstLast(t *testing.T) {
	l := New(selectable("a"), selectable("b"), selectable("c"))
	if !l.IsSelectedFirst() {
		t.Fatal("expected IsSelectedFirst for first item")
	}
	l.SelectLast()
	if !l.IsSelectedLast() {
		t.Fatal("expected IsSelectedLast for last item")
	}
}

func TestVisibleRange(t *testing.T) {
	l := New(selectable("a"), selectable("b"), selectable("c"))
	l.SetSize(40, 2)
	start, end := l.VisibleRange()
	if start != 0 || end != 2 {
		t.Fatalf("VisibleRange() = [%d, %d), want [0, 2)", start, end)
	}
}

func TestEmptyListSelectNextPrev(t *testing.T) {
	l := New()
	if l.SelectNext() {
		t.Fatal("SelectNext on empty should return false")
	}
	if l.SelectPrev() {
		t.Fatal("SelectPrev on empty should return false")
	}
	if l.SelectedItem() != nil {
		t.Fatal("SelectedItem on empty should return nil")
	}
}
