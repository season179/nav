// Package list provides a generic selectable list component with
// keyboard navigation and viewport scrolling.
//
// Items in the list implement the [Item] interface. Items that return
// false from [Item.Selectable] (e.g., section headers, spacers) are
// skipped during keyboard navigation.
package list

// Item is a single entry in a selectable list.
type Item interface {
	// Label returns the display text for filtering and rendering.
	Label() string
	// Selectable reports whether this item can receive selection.
	// Section headers and spacers return false.
	Selectable() bool
}

// List manages an ordered collection of items with single-selection,
// keyboard-style navigation, and a scrolling viewport.
type List struct {
	items    []Item
	selected int
	offset   int
	width    int
	height   int
}

// New creates a List populated with the given items. Selection lands
// on the first selectable item (or -1 when none exist).
func New(items ...Item) *List {
	l := &List{
		items:    items,
		selected: -1,
		width:    40,
		height:   10,
	}
	l.selectFirst()
	return l
}

// SetItems replaces every item in the list and clamps selection to
// the first selectable item.
func (l *List) SetItems(items ...Item) {
	l.items = items
	l.selected = -1
	l.selectFirst()
}

// SetSize configures the viewport dimensions.
func (l *List) SetSize(width, height int) {
	l.width = max(1, width)
	l.height = max(1, height)
}

// Len returns the total number of items.
func (l *List) Len() int { return len(l.items) }

// Selected returns the index of the selected item (-1 when none).
func (l *List) Selected() int { return l.selected }

// SetSelected moves selection to the given index, clamped to valid range.
func (l *List) SetSelected(idx int) {
	if idx < 0 || idx >= len(l.items) {
		l.selected = -1
		return
	}
	l.selected = idx
}

// SelectedItem returns the currently selected item, or nil.
func (l *List) SelectedItem() Item {
	if l.selected < 0 || l.selected >= len(l.items) {
		return nil
	}
	return l.items[l.selected]
}

// SelectFirst moves selection to the first selectable item.
// Returns true when selection changed.
func (l *List) SelectFirst() bool { return l.selectFirst() }

func (l *List) selectFirst() bool {
	for i, item := range l.items {
		if item.Selectable() {
			l.selected = i
			return true
		}
	}
	l.selected = -1
	return false
}

// SelectLast moves selection to the last selectable item.
func (l *List) SelectLast() bool {
	for i := len(l.items) - 1; i >= 0; i-- {
		if l.items[i].Selectable() {
			l.selected = i
			return true
		}
	}
	return false
}

// SelectNext moves selection to the next selectable item, wrapping
// around to the beginning. Returns true when selection moved.
func (l *List) SelectNext() bool {
	if len(l.items) == 0 {
		return false
	}
	for i := l.selected + 1; i < len(l.items); i++ {
		if l.items[i].Selectable() {
			l.selected = i
			return true
		}
	}
	// Wrap around.
	for i := 0; i < len(l.items) && i <= l.selected; i++ {
		if l.items[i].Selectable() {
			l.selected = i
			return true
		}
	}
	return false
}

// SelectPrev moves selection to the previous selectable item,
// wrapping around to the end. Returns true when selection moved.
func (l *List) SelectPrev() bool {
	if len(l.items) == 0 {
		return false
	}
	for i := l.selected - 1; i >= 0; i-- {
		if l.items[i].Selectable() {
			l.selected = i
			return true
		}
	}
	// Wrap around.
	for i := len(l.items) - 1; i >= 0 && i >= l.selected; i-- {
		if l.items[i].Selectable() {
			l.selected = i
			return true
		}
	}
	return false
}

// IsSelectedFirst reports whether the selected item is the first
// selectable item in the list.
func (l *List) IsSelectedFirst() bool {
	for i, item := range l.items {
		if item.Selectable() {
			return l.selected == i
		}
	}
	return false
}

// IsSelectedLast reports whether the selected item is the last
// selectable item in the list.
func (l *List) IsSelectedLast() bool {
	for i := len(l.items) - 1; i >= 0; i-- {
		if l.items[i].Selectable() {
			return l.selected == i
		}
	}
	return false
}

// ScrollToSelected adjusts the viewport offset so the selected item
// is visible.
func (l *List) ScrollToSelected() {
	if l.selected < 0 {
		return
	}
	if l.selected < l.offset {
		l.offset = l.selected
	} else if l.selected >= l.offset+l.height {
		l.offset = l.selected - l.height + 1
	}
	if l.offset < 0 {
		l.offset = 0
	}
}

// ScrollToTop resets the viewport to the beginning.
func (l *List) ScrollToTop() { l.offset = 0 }

// VisibleRange returns the half-open [start, end) range of item
// indices currently in the viewport.
func (l *List) VisibleRange() (int, int) {
	if len(l.items) == 0 {
		return 0, 0
	}
	start := l.offset
	end := start + l.height
	if end > len(l.items) {
		end = len(l.items)
	}
	return start, end
}

// Offset returns the current viewport offset (first visible index).
func (l *List) Offset() int { return l.offset }

// Width returns the viewport width.
func (l *List) Width() int { return l.width }

// Height returns the viewport height.
func (l *List) Height() int { return l.height }
