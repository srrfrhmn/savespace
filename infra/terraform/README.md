# Infrastructure (Terraform)

Phase 0 placeholder. See design doc §8.7 for the target infrastructure.

## Planned layout

```
infra/terraform/
├── modules/
│   ├── network/        # VPC, subnets, NAT, endpoints
│   ├── database/       # RDS PostgreSQL 18, Multi-AZ, PITR, KMS
│   ├── service/        # ECS Fargate service (used by api and worker)
│   ├── edge/           # CloudFront, ALB, WAF, certificates
│   └── security/       # KMS keys, Secrets Manager, IAM roles
└── envs/
    ├── dev/
    ├── staging/
    └── prod/
```

## Rules

- Each environment uses a separate AWS account.
- Every change goes through a pull request with a reviewed `terraform plan`.
- State is in S3 with locking, encrypted with KMS. One state per environment.
- No secrets in Terraform variables or state. Use Secrets Manager.
