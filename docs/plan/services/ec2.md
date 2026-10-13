# EC2

## Crates & scope

- Crates: `aws-sdk-ec2`, plus `aws-sdk-ssm` (session/agent status) and `aws-sdk-cloudwatch` (sparklines)
- Scope: regional

## Resource kinds

| Kind | Aliases | List | Describe |
|---|---|---|---|
| `ec2:instance` | `ec2`, `i` | `DescribeInstances` (paginated) | same, filtered by ID |
| `ec2:sg` | `sg` | `DescribeSecurityGroups` | + `DescribeSecurityGroupRules` |
| `ec2:vpc` | `vpc` | `DescribeVpcs` | |
| `ec2:subnet` | `subnet` | `DescribeSubnets` | |
| `ec2:volume` | `vol`, `ebs` | `DescribeVolumes` | |
| `ec2:ami` | `ami` | `DescribeImages` (`Owners=self`) | |
| `ec2:keypair` | `kp` | `DescribeKeyPairs` | |
| `ec2:eip` | `eip` | `DescribeAddresses` | |

Load-balancer and auto-scaling views are out of scope (different crates); they're
candidates for later.

## Columns (instances)

`NAME` (tag) · `ID` · `STATE` (coloured glyph) · `TYPE` · `PRIVATE IP` · `PUBLIC IP` · `AZ` · `AGE` · `SSM` (●/○)

- The column picker can add any tag as a column.
- Filter syntax: `/web` (text), `/env=prod` (tag), `/state=running`. Tag and state
  filters are sent to the API as `Filters`.

## Actions

| Key | Action | API | Safety | CLI |
|---|---|---|---|---|
| `s` | Shell via SSM | `ssm:StartSession` + `session-manager-plugin` | safe | `aws ssm start-session --target {id}` |
| `S` | SSH (uses public/private IP, `~/.ssh/config`) | shell out to `ssh` | safe | `ssh ec2-user@{ip}` |
| `c` | Console output | `GetConsoleOutput` (base64 → log pane) | safe | `aws ec2 get-console-output --instance-id {id}` |
| `x→start` | Start | `StartInstances` | mutating | `aws ec2 start-instances --instance-ids {id}` |
| `x→stop` | Stop | `StopInstances` | mutating | `aws ec2 stop-instances --instance-ids {id}` |
| `x→reboot` | Reboot | `RebootInstances` | mutating | `aws ec2 reboot-instances --instance-ids {id}` |
| `x→terminate` | Terminate | `TerminateInstances` | destructive | `aws ec2 terminate-instances --instance-ids {id}` |
| `x→tags` | Edit tags | `CreateTags` / `DeleteTags` via editor | mutating | |

- Multi-select works for start, stop and reboot.
- After an action, poll that instance only until its state settles (the SDK's
  `waiters` work here).

## Relations

instance → SG(s), subnet, VPC, volumes, AMI, IAM instance profile → IAM role,
key pair, CloudWatch metrics, SSM managed instance. SG → instances using it.

## Special views

- **Detail** side panel: CPU sparkline (CloudWatch `CPUUtilization`, last 3h) and
  network in/out.
- **SG rules** table: inbound and outbound rules, each with its source/destination
  resolved to a name. An SG referenced as a source can be followed with `g`.

## IAM permissions

- **Read:** `ec2:Describe*`, `ec2:GetConsoleOutput`, `ssm:DescribeInstanceInformation`, `cloudwatch:GetMetricData`
- **Write:** `ec2:Start/Stop/Reboot/TerminateInstances`, `ec2:CreateTags`, `ec2:DeleteTags`, `ssm:StartSession`

## Gotchas

- `DescribeInstances` nests instances inside *reservations*; flatten them.
- `s` (SSM shell) needs the external `session-manager-plugin` binary. Detect it,
  and show install instructions if it's missing. Run it by suspending the TUI
  (leave the alternate screen, hand over the TTY, restore when it exits). The same
  suspend/resume helper is used for `ssh`, `$EDITOR`, `psql` and so on.
- Use `DryRun=true` to pre-check permissions for mutating actions. The error
  `DryRunOperation` means it would succeed.
- Terminated instances stay listed for about an hour. Hide them by default and show
  them with a toggle.
