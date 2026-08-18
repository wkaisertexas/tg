package malformed

func Before() {}

type Broken struct {
	Name string
}

func (broken *Broken) Recover() {
	value := []int{1, 2,, 3}
	_ = value
}
