//! Parsing ARNs (`arn:partition:service:region:account:resource`).

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Arn<'a> {
    pub partition: &'a str,
    pub service: &'a str,
    /// Empty for global services (IAM, S3 buckets).
    pub region: &'a str,
    pub account: &'a str,
    /// Everything after the account: `role/path/name`, `function:name:alias`, …
    pub resource: &'a str,
}

impl<'a> Arn<'a> {
    pub fn parse(s: &'a str) -> Option<Self> {
        let mut parts = s.splitn(6, ':');
        if parts.next()? != "arn" {
            return None;
        }
        Some(Self {
            partition: parts.next()?,
            service: parts.next()?,
            region: parts.next()?,
            account: parts.next()?,
            resource: parts.next()?,
        })
    }

    /// The resource's name: the last `/`-separated part for path-style
    /// resources (`role/service-role/x` → `x`), or the part after the type for
    /// `type:name` ones (`function:x` → `x`, `function:x:alias` → `x`).
    pub fn name(&self) -> &'a str {
        if let Some((_, rest)) = self.resource.split_once('/') {
            return rest.rsplit('/').next().unwrap_or(rest);
        }
        match self.resource.split(':').collect::<Vec<_>>().as_slice() {
            [_, name, ..] => name,
            [only] => only,
            [] => self.resource,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_shapes() {
        let role = Arn::parse("arn:aws:iam::123456789012:role/service-role/thumbs-exec").unwrap();
        assert_eq!(
            (role.service, role.region, role.account),
            ("iam", "", "123456789012")
        );
        assert_eq!(role.name(), "thumbs-exec");

        let function = Arn::parse("arn:aws:lambda:eu-west-1:1:function:thumbs:live").unwrap();
        assert_eq!((function.region, function.name()), ("eu-west-1", "thumbs"));

        let group = Arn::parse("arn:aws:logs:eu-west-1:1:log-group:/aws/lambda/thumbs:*").unwrap();
        assert_eq!(group.service, "logs");

        assert_eq!(
            Arn::parse("arn:aws:s3:::my-bucket").unwrap().name(),
            "my-bucket"
        );
        assert_eq!(
            Arn::parse("arn:aws:sqs:eu-west-1:1:jobs").unwrap().name(),
            "jobs"
        );
        assert!(Arn::parse("not-an-arn").is_none());
        assert!(Arn::parse("arn:aws:iam").is_none());
    }
}
