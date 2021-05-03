use failure::Error;
use quicli::prelude::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use yaml_rust::YamlLoader;

use crate::errors::{FlokiError, FlokiSubprocessExitStatus};

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct BuildSpec {
    name: String,
    #[serde(default = "default_dockerfile")]
    dockerfile: PathBuf,
    #[serde(default = "default_context")]
    context: PathBuf,
    target: Option<String>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct YamlSpec {
    pub file: PathBuf,
    key: String,
}

fn default_dockerfile() -> PathBuf {
    "Dockerfile".into()
}

fn default_context() -> PathBuf {
    ".".into()
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Image {
    Name(String),
    Build { build: BuildSpec },
    Yaml { yaml: YamlSpec },
}

impl Image {
    /// Name of the image
    pub fn name(&self) -> Result<String, Error> {
        match *self {
            Image::Name(ref s) => Ok(s.clone()),
            Image::Build { ref build } => Ok(build.name.clone() + ":floki"),
            Image::Yaml { ref yaml } => {
                let contents = fs::read_to_string(&yaml.file)?;
                let raw = YamlLoader::load_from_str(&contents)?;
                let path = yaml.key.split('.').collect::<Vec<_>>();
                let mut val = &raw[0];

                for key in &path {
                    // Yaml arrays and maps with scalar keys can both be indexed by
                    // usize, so heuristically prefer a usize index to a &str index.
                    val = match key.parse::<usize>() {
                        Ok(x) => &val[x],
                        Err(_) => &val[*key],
                    };
                }
                val.as_str()
                    .map(std::string::ToString::to_string)
                    .ok_or_else(|| {
                        FlokiError::FailedToFindYamlKey {
                            key: yaml.key.to_string(),
                            file: yaml.file.display().to_string(),
                        }
                        .into()
                    })
            }
        }
    }

    /// Do the required work to get the image, and then return
    /// it's name
    pub fn obtain_image(&self, floki_root: &Path) -> Result<String, Error> {
        match *self {
            // Deal with the case where want to build an image
            Image::Build { ref build } => {
                let mut command = Command::new("docker");
                command
                    .arg("build")
                    .arg("-t")
                    .arg(self.name()?)
                    .arg("-f")
                    .arg(&floki_root.join(&build.dockerfile));

                if let Some(target) = &build.target {
                    command.arg("--target").arg(target);
                }

                let exit_status = command
                    .arg(&floki_root.join(&build.context))
                    .spawn()?
                    .wait()?;
                if exit_status.success() {
                    Ok(self.name()?)
                } else {
                    Err(FlokiError::FailedToBuildImage {
                        image: self.name()?,
                        exit_status: FlokiSubprocessExitStatus {
                            process_description: "docker build".into(),
                            exit_status,
                        },
                    }
                    .into())
                }
            }
            // All other cases we just return the name
            _ => Ok(self.name()?),
        }
    }
}

// Now we have some functions which are useful in general

/// Wrapper to pull an image by it's name
pub fn pull_image(name: &str) -> Result<(), Error> {
    debug!("Pulling image: {}", name);
    let exit_status = Command::new("docker")
        .arg("pull")
        .arg(name)
        .spawn()?
        .wait()?;

    if exit_status.success() {
        Ok(())
    } else {
        Err(FlokiError::FailedToPullImage {
            image: name.into(),
            exit_status: FlokiSubprocessExitStatus {
                process_description: "docker pull".into(),
                exit_status,
            },
        }
        .into())
    }
}

/// Determine whether an image exists locally
pub fn image_exists_locally(name: &str) -> Result<bool, Error> {
    let ret = Command::new("docker")
        .args(&["history", name])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| FlokiError::FailedToCheckForImage {
            image: name.to_string(),
            error: e,
        })?;

    Ok(ret.code() == Some(0))
}

#[cfg(test)]
mod test {
    use super::*;
    use which::which;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct TestImage {
        image: Image,
    }

    #[test]
    fn test_image_spec_by_string() {
        let yaml = "image: foo";
        let expected = TestImage {
            image: Image::Name("foo".into()),
        };
        let actual: TestImage = serde_yaml::from_str(yaml).unwrap();
        assert!(actual == expected);
    }

    #[test]
    fn test_image_spec_by_build_spec() {
        let yaml = "image:\n  build:\n    name: foo\n    dockerfile: Dockerfile.test \n    context: ./context\n    target: builder";
        let expected = TestImage {
            image: Image::Build {
                build: BuildSpec {
                    name: "foo".into(),
                    dockerfile: "Dockerfile.test".into(),
                    context: "./context".into(),
                    target: Some("builder".into()),
                },
            },
        };
        let actual: TestImage = serde_yaml::from_str(yaml).unwrap();
        assert!(actual == expected);
    }

    /// Determine if a given program is installed in the current environment.
    fn program_is_installed(program: &str) -> bool {
        which(program).is_ok()
    }

    #[test]
    fn test_image_exists_locally() {
        // Need to test if "docker" command is available, otherwise test will
        // fail.
        assert!(
            program_is_installed("docker"),
            "docker required for this test but not installed!"
        );

        // First pull an image that is known to exist, then check that we
        // correctly report it as existing. This also acts as a regression test
        // against this exact image being hard-coded in the function (which it
        // was previously!), as pulling here means that the second subtest
        // below would then fail.
        let existent_image = "docker:stable-dind";
        pull_image(existent_image).unwrap();
        assert!(image_exists_locally(existent_image).unwrap());

        // Now test an image that doesn't exist, and therefore shouldn't
        // exist locally.
        let non_existent_image = "doesnt_exist:re4lly--sh0u1dnt-exist";
        assert!(!image_exists_locally(non_existent_image).unwrap());
    }
}
