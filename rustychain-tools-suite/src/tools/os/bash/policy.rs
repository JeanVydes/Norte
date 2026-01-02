use std::{collections::HashSet, path::PathBuf};

/// Security policy for command execution
#[derive(Clone, Debug)]
pub struct SecurityPolicy {
    /// Allowed commands (whitelist). If empty, all commands allowed.
    pub allowed_commands: HashSet<String>,
    /// Blocked commands (blacklist). Takes precedence over whitelist.
    pub blocked_commands: HashSet<String>,
    /// Allowed working directories. If empty, all directories allowed.
    pub allowed_directories: HashSet<PathBuf>,
    /// Whether to allow shell execution (pipes, redirects, etc.)
    pub allow_shell: bool,
    /// Maximum execution time in seconds
    pub max_timeout_secs: u64,
    /// Whether to require approval for destructive commands
    pub require_approval_for_destructive: bool,
    /// Enable audit logging
    pub audit_logging: bool,
}

impl Default for SecurityPolicy {
    fn default() -> Self {
        Self {
            allowed_commands: HashSet::new(),
            blocked_commands: Self::default_blocked_commands(),
            allowed_directories: HashSet::new(),
            allow_shell: false, // Disabled by default for security
            max_timeout_secs: 300,
            require_approval_for_destructive: true,
            audit_logging: true,
        }
    }
}

impl SecurityPolicy {
    /// Check if a command is allowed by this policy
    pub fn is_command_allowed(&self, command: &str) -> Result<(), String> {
        let cmd_base = command.split_whitespace().next().unwrap_or(command);

        // Check blocklist first
        if self.blocked_commands.contains(cmd_base) {
            return Err(format!(
                "Command '{}' is blocked by security policy",
                cmd_base
            ));
        }

        // Check whitelist if configured
        if !self.allowed_commands.is_empty() && !self.allowed_commands.contains(cmd_base) {
            return Err(format!("Command '{}' is not in the allowed list", cmd_base));
        }

        Ok(())
    }

    /// Check if a directory is allowed
    pub fn is_directory_allowed(&self, path: &str) -> Result<(), String> {
        if self.allowed_directories.is_empty() {
            return Ok(());
        }

        let path_buf = PathBuf::from(path);
        for allowed in &self.allowed_directories {
            if path_buf.starts_with(allowed) {
                return Ok(());
            }
        }

        Err(format!(
            "Directory '{}' is not allowed by security policy",
            path
        ))
    }

    /// Check if command appears to be destructive
    pub fn is_potentially_destructive(&self, command: &str, args: &[String]) -> bool {
        let dangerous_patterns = [
            "rm", "delete", "del", "format", "drop", "truncate", "destroy",
        ];
        let full_cmd = format!("{} {}", command, args.join(" ")).to_lowercase();

        dangerous_patterns.iter().any(|p| full_cmd.contains(p))
    }

    pub fn require_approval(&self, command: &str, args: &[String]) -> bool {
        if self.require_approval_for_destructive {
            self.is_potentially_destructive(command, args)
        } else {
            false
        }
    }

    pub fn add_allowed_command(mut self, command: &str) -> Self {
        self.allowed_commands.insert(command.to_string());
        self
    }

    pub fn add_allowed_directory(mut self, path: PathBuf) -> Self {
        self.allowed_directories.insert(path);
        self
    }

    pub fn add_blocked_command(mut self, command: &str) -> Self {
        self.blocked_commands.insert(command.to_string());
        self
    }

    pub fn set_max_timeout(mut self, secs: u64) -> Self {
        self.max_timeout_secs = secs;
        self
    }

    pub fn enable_audit_logging(mut self, enable: bool) -> Self {
        self.audit_logging = enable;
        self
    }

    pub fn allow_shell_execution(mut self, allow: bool) -> Self {
        self.allow_shell = allow;
        self
    }

    pub fn require_approval_for_destructive_commands(mut self, require: bool) -> Self {
        self.require_approval_for_destructive = require;
        self
    }

    /// Get default set of dangerous commands to block
    pub fn default_blocked_commands() -> HashSet<String> {
        [
            // File/Directory Destruction
            "rm",
            "rmdir",
            "del",
            "erase",
            "shred",
            "wipe",
            "srm",
            "dd",
            "truncate",
            "unlink",
            // Disk Operations
            "format",
            "mkfs",
            "mkfs.ext4",
            "mkfs.xfs",
            "mkfs.btrfs",
            "fdisk",
            "parted",
            "gdisk",
            "cfdisk",
            "sfdisk",
            "hdparm",
            "blkdiscard",
            "wipefs",
            // System Control
            "reboot",
            "shutdown",
            "halt",
            "poweroff",
            "init",
            "telinit",
            "systemctl",
            "service",
            // Process Management (dangerous)
            "kill",
            "killall",
            "pkill",
            "killall5",
            "fuser",
            "xkill",
            "destroy",
            // User/Permission Management
            "userdel",
            "groupdel",
            "passwd",
            "chpasswd",
            "usermod",
            "groupmod",
            "deluser",
            "delgroup",
            "chmod",
            "chown",
            "chgrp",
            "chattr",
            "setfacl",
            // Package Management (can break system)
            "apt-get",
            "apt",
            "yum",
            "dnf",
            "zypper",
            "pacman",
            "dpkg",
            "rpm",
            "snap",
            "flatpak",
            "pip",
            "npm",
            "cargo",
            "gem",
            "brew",
            // Network/Firewall (security risk)
            "iptables",
            "ip6tables",
            "nftables",
            "ufw",
            "firewalld",
            "ifconfig",
            "route",
            "tc",
            "ethtool",
            // Mounting/Unmounting
            "mount",
            "umount",
            "unmount",
            "swapon",
            "swapoff",
            // Kernel/Module Operations
            "modprobe",
            "insmod",
            "rmmod",
            "depmod",
            "kexec",
            "renice",
            // Cron/Scheduled Tasks
            "crontab",
            "at",
            "batch",
            // Secure Deletion Tools
            "cipher",
            "sdelete",
            "secure-delete",
            // Database Commands (destructive)
            "drop",
            "delete",
            "truncate",
            "destroy",
            "dropdb",
            "mysql",
            "psql",
            "mongo",
            // Container/VM Management
            "docker",
            "podman",
            "kubectl",
            "virsh",
            "vboxmanage",
            // Boot/GRUB
            "grub-install",
            "update-grub",
            "grub2-install",
            "efibootmgr",
            // SELinux/AppArmor
            "setenforce",
            "semanage",
            "aa-enforce",
            "aa-complain",
            // System Configuration
            "sysctl",
            "systemd-run",
            "hostnamectl",
            "timedatectl",
            "localectl",
            // Crypto/Encryption (misuse risk)
            "cryptsetup",
            "luks",
            "veracrypt",
            // Backup/Restore (can overwrite)
            "restore",
            "dump",
            "tar",
            "rsync",
            "cp",
            "mv",
            // Partition Table
            "sgdisk",
            "partprobe",
            "blockdev",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }
}
