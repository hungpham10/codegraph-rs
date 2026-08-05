package main

type UserService struct {
    Name string
}

func (u *UserService) Greet() string {
    return "Hello, " + u.Name
}

func main() {
    svc := UserService{Name: "Alice"}
    svc.Greet()
}