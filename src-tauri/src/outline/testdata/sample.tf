variable "region" {
  default = "us-east-1"
}
resource "aws_instance" "web" {
  ami = "ami-123"
  tags = {
    Name = "web"
  }
}
locals { x = 1 }
