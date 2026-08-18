package sample

const Default, Alternate = 1, 2
var Current = Service{}

type Embedded struct{}

type Service struct {
	Name string
	Count, Total int
	Embedded
}

type Reader interface {
	Read([]byte) (int, error)
	Close() error
}

type Alias = Service
type Identifier string

func Render(value string) string {
	type Hidden struct{}
	local := value
	return local
}

func (service *Service) Save() {}

func (service Service) Render() string {
	return service.Name
}

type Café struct {
	Crème string
}
