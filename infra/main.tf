terraform {
  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 6.0"
    }
  }
}

provider "aws" {
  region = "eu-north-1"
}

data "aws_caller_identity" "current" {}

resource "aws_vpc" "main" {
  cidr_block           = "10.10.0.0/16"
  enable_dns_support   = true
  enable_dns_hostnames = true

  tags = {
    Name = "token-tracker"
  }
}

resource "aws_subnet" "public" {
  vpc_id            = aws_vpc.main.id
  cidr_block        = "10.10.1.0/24"
  availability_zone = "eu-north-1a"

  tags = {
    Name = "token-tracker-public"
  }
}

resource "aws_internet_gateway" "main" {
  vpc_id = aws_vpc.main.id

  tags = {
    Name = "token-tracker"
  }
}

resource "aws_route_table" "public" {
  vpc_id = aws_vpc.main.id

  route {
    cidr_block = "0.0.0.0/0"
    gateway_id = aws_internet_gateway.main.id
  }

  tags = {
    Name = "token-tracker-public"
  }
}

resource "aws_route_table_association" "public" {
  subnet_id      = aws_subnet.public.id
  route_table_id = aws_route_table.public.id
}

resource "aws_security_group" "server" {
  name        = "token-tracker-server"
  description = "Public web access; administration through SSM"
  vpc_id      = aws_vpc.main.id

  tags = {
    Name = "token-tracker-server"
  }
}

resource "aws_vpc_security_group_ingress_rule" "http" {
  security_group_id = aws_security_group.server.id
  description       = "HTTP redirects and certificate validation"

  cidr_ipv4   = "0.0.0.0/0"
  ip_protocol = "tcp"
  from_port   = 80
  to_port     = 80
}

resource "aws_vpc_security_group_ingress_rule" "https" {
  security_group_id = aws_security_group.server.id
  description       = "HTTPS access to Caddy"

  cidr_ipv4   = "0.0.0.0/0"
  ip_protocol = "tcp"
  from_port   = 443
  to_port     = 443
}

resource "aws_vpc_security_group_egress_rule" "outbound" {
  security_group_id = aws_security_group.server.id
  description       = "Outbound access for SSM, updates, and certificates"

  cidr_ipv4   = "0.0.0.0/0"
  ip_protocol = "-1"
}

resource "aws_iam_role" "server" {
  name = "token-tracker-server"

  assume_role_policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect = "Allow"
      Principal = {
        Service = "ec2.amazonaws.com"
      }
      Action = "sts:AssumeRole"
    }]
  })
}

resource "aws_iam_role_policy_attachment" "ssm" {
  role       = aws_iam_role.server.name
  policy_arn = "arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore"
}

resource "aws_iam_role_policy" "deployment_downloads" {
  name = "token-tracker-deployment-downloads"
  role = aws_iam_role.server.name

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect   = "Allow"
      Action   = "s3:GetObject"
      Resource = "${aws_s3_bucket.deployments.arn}/*"
    }]
  })
}

resource "aws_iam_instance_profile" "server" {
  name = "token-tracker-server"
  role = aws_iam_role.server.name
}

resource "aws_ebs_volume" "data" {
  availability_zone = aws_subnet.public.availability_zone
  type              = "gp3"
  size              = 10
  encrypted         = true

  tags = {
    Name = "token-tracker-data"
  }
}

resource "aws_volume_attachment" "data" {
  device_name = "/dev/sdf"
  volume_id   = aws_ebs_volume.data.id
  instance_id = aws_instance.server.id

  stop_instance_before_detaching = true
}

data "aws_ssm_parameter" "amazon_linux" {
  name = "/aws/service/ami-amazon-linux-latest/al2023-ami-kernel-6.1-x86_64"
}

resource "aws_instance" "server" {
  ami           = data.aws_ssm_parameter.amazon_linux.value
  instance_type = "t3.small"

  subnet_id                   = aws_subnet.public.id
  vpc_security_group_ids      = [aws_security_group.server.id]
  associate_public_ip_address = false
  iam_instance_profile        = aws_iam_instance_profile.server.name

  root_block_device {
    volume_type           = "gp3"
    volume_size           = 20
    encrypted             = true
    delete_on_termination = true
  }

  metadata_options {
    http_endpoint = "enabled"
    http_tokens   = "required"
  }

  credit_specification {
    cpu_credits = "standard"
  }

  tags = {
    Name = "token-tracker"
  }

  user_data = templatefile("${path.module}/user-data.sh.tftpl", {
    volume_id = aws_ebs_volume.data.id
  })

  user_data_replace_on_change = true

  lifecycle {
    # The provider reports true after the Elastic IP is attached.
    ignore_changes = [associate_public_ip_address]
  }
}

resource "aws_eip" "server" {
  domain = "vpc"

  tags = {
    Name = "token-tracker"
  }
}

resource "aws_eip_association" "server" {
  instance_id   = aws_instance.server.id
  allocation_id = aws_eip.server.id

  depends_on = [aws_internet_gateway.main]
}

resource "aws_s3_bucket" "deployments" {
  bucket_prefix = "token-tracker-deployments-"
  force_destroy = true
}

resource "aws_s3_bucket_public_access_block" "deployments" {
  bucket = aws_s3_bucket.deployments.id

  block_public_acls       = true
  block_public_policy     = true
  ignore_public_acls      = true
  restrict_public_buckets = true
}

output "deployment_bucket_name" {
  value = aws_s3_bucket.deployments.id
}

output "aws_account_id" {
  value = data.aws_caller_identity.current.account_id
}

output "instance_id" {
  value = aws_instance.server.id
}

output "public_ip" {
  value = aws_eip.server.public_ip
}

output "data_volume_id" {
  value = aws_ebs_volume.data.id
}
