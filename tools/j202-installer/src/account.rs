//! The account created at the first boot: checked here, before anything is written.

#[derive(Default)]
pub struct Account {
    pub user: String,
    pub password: String,
    pub confirm: String,
    pub hostname: String,
    pub move_to_nvme: bool,
}

pub fn check_user(user: &str) -> Result<(), &'static str> {
    let mut chars = user.chars();
    match chars.next() {
        None => return Err("Entre un nom d'utilisateur"),
        Some(c) if !(c.is_ascii_lowercase() || c == '_') => {
            return Err("Commence par une lettre minuscule ou _");
        }
        _ => {}
    }
    if user.len() > 32 {
        return Err("32 caractères au plus");
    }
    if !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-') {
        return Err("Lettres minuscules, chiffres, _ et - seulement");
    }
    if matches!(user, "root" | "bin" | "daemon" | "nobody" | "alarm") {
        return Err("Ce nom est déjà pris par le système");
    }
    Ok(())
}

pub fn check_password(password: &str, confirm: &str) -> Result<(), &'static str> {
    if password.chars().count() < 8 {
        return Err("8 caractères au moins");
    }
    if password != confirm {
        return Err("Les deux mots de passe diffèrent");
    }
    Ok(())
}

pub fn check_hostname(name: &str) -> Result<(), &'static str> {
    if name.is_empty() || name.len() > 63 {
        return Err("De 1 à 63 caractères");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err("Lettres minuscules, chiffres et - seulement");
    }
    if name.starts_with('-') || name.ends_with('-') {
        return Err("Pas de - au début ni à la fin");
    }
    Ok(())
}

impl Account {
    pub fn new() -> Self {
        Self {
            hostname: "jetson-nano".into(),
            move_to_nvme: true,
            ..Default::default()
        }
    }

    pub fn is_valid(&self) -> bool {
        check_user(&self.user).is_ok()
            && check_password(&self.password, &self.confirm).is_ok()
            && check_hostname(&self.hostname).is_ok()
    }

    /// The /boot/j202-firstboot.conf the installer writes for the board's first-boot service. The
    /// password is a SHA-512 crypt hash, never plain text; nothing here is a secret the image keeps.
    pub fn firstboot_conf(&self) -> Result<String, String> {
        let hash = hash_password(&self.password)?;
        let nvme = if self.move_to_nvme { 1 } else { 0 };
        Ok(format!(
            "# written by j202-installer; removed by the first-boot service\n\
             J202_USER={}\n\
             J202_HOST={}\n\
             J202_PASS_HASH='{}'\n\
             J202_NVME={}\n",
            self.user, self.hostname, hash, nvme
        ))
    }
}

/// A SHA-512 crypt hash ($6$…), as /etc/shadow and `usermod -p` expect.
pub fn hash_password(password: &str) -> Result<String, String> {
    use sha_crypt::{PasswordHasher, ShaCrypt};
    // `sha-crypt` encodes the provided salt bytes with crypt64 internally: 12 random bytes encode
    // to 16 crypt64 characters, matching Linux libxcrypt's 16-char SHA-512 salt limit.
    let mut raw = [0u8; 12];
    getrandom::fill(&mut raw).map_err(|e| format!("génération du sel : {e}"))?;
    let hash = ShaCrypt::SHA512
        .hash_password_with_salt(password.as_bytes(), &raw)
        .map_err(|e| format!("hachage du mot de passe : {e}"))?;
    Ok(hash.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn users() {
        assert!(check_user("kevin").is_ok());
        assert!(check_user("_svc-1").is_ok());
        assert!(check_user("").is_err());
        assert!(check_user("Kevin").is_err());
        assert!(check_user("1abc").is_err());
        assert!(check_user("a b").is_err());
        assert!(check_user("root").is_err());
        assert!(check_user(&"a".repeat(33)).is_err());
    }

    #[test]
    fn passwords() {
        assert!(check_password("12345678", "12345678").is_ok());
        assert!(check_password("1234567", "1234567").is_err());
        assert!(check_password("12345678", "12345679").is_err());
        assert!(check_password("éééééééé", "éééééééé").is_ok());
    }

    #[test]
    fn hostnames() {
        assert!(check_hostname("jetson-nano").is_ok());
        assert!(check_hostname("-a").is_err());
        assert!(check_hostname("a-").is_err());
        assert!(check_hostname("A").is_err());
        assert!(check_hostname("").is_err());
    }

    #[test]
    fn hash_is_sha512_crypt_and_verifies() {
        use sha_crypt::{PasswordHash, PasswordVerifier, ShaCrypt};
        use std::str::FromStr;
        let h = hash_password("hunter2good").unwrap();
        assert!(h.starts_with("$6$"));
        let parts: Vec<&str> = h.split('$').collect();
        assert_eq!(parts.len(), 5, "unexpected crypt format: {h}");
        assert_eq!(parts[1], "6");
        assert!(
            parts[3].len() <= 16 && parts[3].len() == 16,
            "bad salt len: {}",
            parts[3].len()
        );
        assert_eq!(parts[4].len(), 86, "bad digest len: {}", parts[4].len());
        let parsed = PasswordHash::from_str(&h).unwrap();
        assert!(ShaCrypt::SHA512.verify_password(b"hunter2good", &parsed).is_ok());
        assert!(ShaCrypt::SHA512.verify_password(b"wrong", &parsed).is_err());
        // a fresh salt each time
        assert_ne!(hash_password("hunter2good").unwrap(), h);
    }

    #[test]
    fn firstboot_conf_has_user_host_and_hash() {
        let mut a = Account::new();
        a.user = "kevin".into();
        a.hostname = "nano".into();
        a.password = "hunter2good".into();
        a.confirm = "hunter2good".into();
        let c = a.firstboot_conf().unwrap();
        assert!(c.contains("J202_USER=kevin"));
        assert!(c.contains("J202_HOST=nano"));
        assert!(c.contains("J202_PASS_HASH='$6$"));
        assert!(c.contains("J202_NVME=1"));

        a.move_to_nvme = false;
        let c2 = a.firstboot_conf().unwrap();
        assert!(c2.contains("J202_NVME=0"));
    }
}
