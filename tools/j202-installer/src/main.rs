//! Installeur J202: installs jetson-nano-omarchy on a reComputer J1020 v2 (Jetson Nano, J202 carrier)
//! from Windows, through the recovery mode, without WSL.

#![windows_subsystem = "windows"]

use j202_installer::{account, download, driver, install, release, usb};

use eframe::egui::{self, Color32, RichText};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Step {
    Prepare,
    Account,
    Recovery,
    Install,
    Done,
}

impl Step {
    const ALL: [Step; 5] = [Step::Prepare, Step::Account, Step::Recovery, Step::Install, Step::Done];

    fn title(self) -> &'static str {
        match self {
            Step::Prepare => "Préparation",
            Step::Account => "Compte",
            Step::Recovery => "Recovery",
            Step::Install => "Installation",
            Step::Done => "Terminé",
        }
    }
}

type Shared<T> = Arc<Mutex<Option<T>>>;

struct App {
    step: Step,
    cache: PathBuf,
    releases: Shared<Result<Vec<release::Release>, String>>,
    selected: usize,
    /// A release chosen from a folder on disk, shown as an extra option alongside GitHub's.
    local: Option<release::Release>,
    use_local: bool,
    license: bool,
    download: Option<download::Task>,
    account: account::Account,
    apx: Arc<Mutex<usb::Recovery>>,
    verify_flash: bool,
    confirm_erase: bool,
    run: Option<install::Run>,
    show_log: bool,
    driver_msg: Option<Result<String, String>>,
    /// Set when the elevated copy was started to resume the driver install (one click, one UAC).
    resume_driver: bool,
}

fn fetch_releases(ctx: &egui::Context) -> Shared<Result<Vec<release::Release>, String>> {
    let slot = Arc::new(Mutex::new(None));
    let (s, ctx) = (slot.clone(), ctx.clone());
    std::thread::spawn(move || {
        *s.lock().unwrap() = Some(release::fetch());
        ctx.request_repaint();
    });
    slot
}

fn mib(bytes: u64) -> String {
    format!("{:.0} Mo", bytes as f64 / 1_048_576.0)
}

const OK: Color32 = Color32::from_rgb(0x3b, 0x9a, 0x3b);
const WARN: Color32 = Color32::from_rgb(0xc0, 0x7a, 0x10);
const ERR: Color32 = Color32::from_rgb(0xd0, 0x40, 0x40);

impl App {
    fn new(cc: &eframe::CreationContext) -> Self {
        let cache = directories::ProjectDirs::from("", "", "j202-installer")
            .map(|p| p.cache_dir().to_path_buf())
            .unwrap_or_else(|| std::env::temp_dir().join("j202-installer"));
        Self {
            step: Step::Prepare,
            cache,
            releases: fetch_releases(&cc.egui_ctx),
            selected: 0,
            local: None,
            use_local: false,
            license: false,
            download: None,
            account: account::Account::new(),
            apx: usb::watch(cc.egui_ctx.clone()),
            verify_flash: false,
            confirm_erase: false,
            run: None,
            show_log: false,
            driver_msg: None,
            resume_driver: std::env::args().any(|a| a == "--install-driver"),
        }
    }

    fn release(&self) -> Option<release::Release> {
        if self.use_local {
            return self.local.clone();
        }
        match &*self.releases.lock().unwrap() {
            Some(Ok(list)) => list.get(self.selected).cloned(),
            _ => None,
        }
    }

    /// The NVIDIA L4T archive: taken from the local folder if it is there, else downloaded.
    fn l4t(&self, r: &release::Release) -> download::Item {
        if let Some(path) = r.local_l4t() {
            let size = std::fs::metadata(&path).map(|m| m.len()).ok();
            return download::Item {
                url: String::new(),
                dest: path,
                size,
            };
        }
        download::Item {
            url: download::L4T_URL.into(),
            dest: self.cache.join("nvidia").join(download::L4T_NAME),
            size: None,
        }
    }

    fn release_files(&self, r: &release::Release) -> Vec<download::Item> {
        r.assets
            .iter()
            // the L4T archive, when present locally, is handled by l4t(), not downloaded as an asset
            .filter(|a| !(a.name.starts_with(release::L4T_PREFIX) && a.name.ends_with("_aarch64.tbz2")))
            .map(|a| match &r.local_dir {
                Some(dir) => download::Item {
                    url: String::new(),
                    dest: dir.join(&a.name),
                    size: Some(a.size),
                },
                None => download::Item {
                    url: a.browser_download_url.clone(),
                    dest: self.cache.join("releases").join(&r.tag).join(&a.name),
                    size: Some(a.size),
                },
            })
            .collect()
    }

    fn downloading(&self) -> bool {
        self.download
            .as_ref()
            .is_some_and(|t| t.progress.lock().unwrap().result.is_none())
    }

    fn files_ready(&self, r: &release::Release) -> bool {
        self.l4t(r).is_done() && self.release_files(r).iter().all(|f| f.is_done())
    }

    /// Opens a folder dialog and, if it holds a release, selects it.
    fn pick_local(&mut self) {
        if let Some(dir) = rfd::FileDialog::new()
            .set_title("Dossier de la release J202")
            .pick_folder()
        {
            match release::local(&dir) {
                Ok(r) => {
                    self.local = Some(r);
                    self.use_local = true;
                    self.download = None;
                }
                Err(e) => self.driver_msg = Some(Err(e)),
            }
        }
    }

    /// Installs the WinUSB driver, elevating first if need be (Windows asks the user).
    fn install_driver(&mut self) {
        if driver::is_elevated() {
            self.driver_msg = Some(
                driver::install(&self.cache)
                    .map(|()| "Pilote installé. Windows peut mettre quelques secondes à le prendre.".into()),
            );
        } else {
            match driver::relaunch_as_admin() {
                Ok(()) => std::process::exit(0),
                Err(e) => self.driver_msg = Some(Err(e)),
            }
        }
    }

    /// The driver row, shown on more than one page: status, and the install button when it is needed.
    fn driver_row(&mut self, ui: &mut egui::Ui) {
        match &*self.apx.lock().unwrap() {
            usb::Recovery::Ready(a) => {
                ui.colored_label(OK, format!("✔ pilote {} actif", a.driver.as_deref().unwrap_or("")));
                return;
            }
            usb::Recovery::NeedsDriver(d) => {
                ui.colored_label(
                    WARN,
                    format!("⚠ pilote à installer (actuel : {})", d.as_deref().unwrap_or("aucun")),
                );
            }
            usb::Recovery::Absent => {
                ui.weak("carte non détectée en recovery");
                return;
            }
        }
        if ui.button("Installer le pilote WinUSB").clicked() {
            self.install_driver();
        }
        match &self.driver_msg {
            Some(Ok(m)) => {
                ui.colored_label(OK, m);
            }
            Some(Err(e)) => {
                ui.colored_label(ERR, e);
            }
            None => {}
        }
    }

    fn sidebar(&self, ui: &mut egui::Ui) {
        ui.add_space(12.0);
        let now = Step::ALL.iter().position(|s| *s == self.step).unwrap_or(0);
        for (i, s) in Step::ALL.iter().enumerate() {
            let text = if i < now {
                RichText::new(format!("✔  {}", s.title())).color(OK)
            } else if i == now {
                RichText::new(format!("▶  {}", s.title())).strong()
            } else {
                RichText::new(format!("    {}", s.title())).weak()
            };
            ui.label(text.size(15.0));
            ui.add_space(6.0);
        }
    }

    fn prepare(&mut self, ui: &mut egui::Ui) {
        ui.heading("Préparation");
        ui.add_space(8.0);

        let gh = self.releases.lock().unwrap().clone();
        let list: Vec<release::Release> = match &gh {
            Some(Ok(l)) => l.clone(),
            _ => Vec::new(),
        };
        let busy = self.downloading();

        // version row: the GitHub list and a local-folder button
        ui.horizontal(|ui| {
            ui.label("Version à installer");
            let shown = match self.release() {
                Some(r) if self.use_local => format!("{} (local)", r.tag),
                Some(r) => r.tag,
                None => "—".into(),
            };
            ui.add_enabled_ui(!busy, |ui| {
                egui::ComboBox::from_id_salt("version")
                    .width(200.0)
                    .selected_text(shown)
                    .show_ui(ui, |ui| {
                        for (i, rel) in list.iter().enumerate() {
                            let mut name = rel.tag.clone();
                            if i == 0 {
                                name.push_str(" (dernière)");
                            }
                            if rel.prerelease {
                                name.push_str(" (essai)");
                            }
                            if ui
                                .selectable_label(!self.use_local && self.selected == i, name)
                                .clicked()
                            {
                                self.selected = i;
                                self.use_local = false;
                            }
                        }
                        if let Some(loc) = self.local.clone()
                            && ui
                                .selectable_label(self.use_local, format!("{} (local)", loc.tag))
                                .clicked()
                        {
                            self.use_local = true;
                        }
                    });
            });
            if ui.add_enabled(!busy, egui::Button::new("Fichier local…")).clicked() {
                self.pick_local();
            }
        });
        if let Some(Err(e)) = &gh {
            ui.colored_label(WARN, format!("GitHub : {e} — tu peux choisir un fichier local."));
        } else if gh.is_none() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.weak("lecture des releases sur GitHub…");
            });
        }
        ui.add_space(8.0);

        let Some(r) = self.release() else {
            ui.weak("Choisis une version GitHub ou un dossier de release local.");
            return;
        };

        let l4t_done = self.l4t(&r).is_done();
        let files_done = self.release_files(&r).iter().all(|f| f.is_done());
        let is_local = r.local_dir.is_some();

        egui::Grid::new("prep")
            .num_columns(2)
            .spacing([24.0, 10.0])
            .show(ui, |ui| {
                ui.label("Système");
                if r.image().is_some() {
                    ui.colored_label(OK, "✔ image système présente");
                } else {
                    ui.colored_label(WARN, "⚠ pas d'image système dans cette release");
                }
                ui.end_row();

                ui.label("Fichiers NVIDIA (L4T R32.7.6)");
                if r.local_l4t().is_some() {
                    ui.colored_label(OK, "✔ dans le dossier");
                } else if l4t_done {
                    ui.colored_label(OK, "✔ téléchargés");
                } else {
                    ui.weak("à télécharger chez NVIDIA");
                }
                ui.end_row();

                ui.label("Fichiers de la release");
                if files_done {
                    ui.colored_label(
                        OK,
                        if is_local {
                            "✔ dans le dossier"
                        } else {
                            "✔ téléchargés"
                        },
                    );
                } else {
                    ui.weak(format!("à télécharger, {}", mib(r.size())));
                }
                ui.end_row();
            });
        ui.add_space(8.0);

        ui.label("Pilote USB pour APX");
        self.driver_row(ui);
        ui.add_space(10.0);

        let needs_license = !(is_local && r.local_l4t().is_some());
        if needs_license {
            ui.horizontal(|ui| {
                ui.add_enabled(
                    !busy,
                    egui::Checkbox::new(
                        &mut self.license,
                        "J'accepte la licence du logiciel NVIDIA (Linux for Tegra)",
                    ),
                );
                ui.hyperlink_to("Lire", "https://developer.nvidia.com/embedded/jetson-linux-archive");
            });
        }

        ui.add_space(8.0);
        if let Some(t) = &self.download {
            let p = t.progress.lock().unwrap();
            match &p.result {
                None => {
                    let frac = p.total.map(|tot| p.done as f32 / tot.max(1) as f32);
                    let mut bar = egui::ProgressBar::new(frac.unwrap_or(0.0)).text(format!(
                        "{} ({}/{}) : {}",
                        p.file,
                        p.index + 1,
                        p.count,
                        mib(p.done)
                    ));
                    if frac.is_none() {
                        bar = bar.animate(true);
                    }
                    ui.add(bar);
                }
                Some(Err(e)) => {
                    ui.colored_label(ERR, e);
                }
                Some(Ok(())) => {}
            }
        }

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let ready = self.files_ready(&r);
            // local releases need no download; GitHub ones need the licence accepted
            if busy {
                if ui.button("Annuler le téléchargement").clicked()
                    && let Some(t) = &self.download
                {
                    t.cancel();
                }
            } else if !ready && (!is_local || (files_done && !l4t_done)) {
                if ui.add_enabled(self.license, egui::Button::new("Télécharger")).clicked() {
                    let mut items = vec![self.l4t(&r)];
                    if !is_local {
                        items.extend(self.release_files(&r));
                    }
                    self.download = Some(download::start(items, ui.ctx().clone()));
                }
                if !self.license {
                    ui.weak("Accepte la licence NVIDIA pour télécharger.");
                }
            } else if !ready && is_local {
                ui.colored_label(WARN, "Ce dossier n'a pas tous les fichiers de la release.");
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let ok = ready && !busy && r.image().is_some() && (!needs_license || self.license);
                if ui.add_enabled(ok, egui::Button::new("Suivant")).clicked() {
                    self.step = Step::Account;
                }
            });
        });
    }

    fn account(&mut self, ui: &mut egui::Ui) {
        ui.heading("Compte");
        ui.label("Créé au premier démarrage de la carte.");
        ui.add_space(12.0);
        let a = &mut self.account;
        let err = |ui: &mut egui::Ui, r: Result<(), &str>, empty: bool| {
            if let (Err(e), false) = (r, empty) {
                ui.colored_label(ERR, e);
            } else {
                ui.label("");
            }
        };
        egui::Grid::new("account")
            .num_columns(3)
            .spacing([16.0, 10.0])
            .show(ui, |ui| {
                ui.label("Nom d'utilisateur");
                ui.add(egui::TextEdit::singleline(&mut a.user).desired_width(200.0));
                err(ui, account::check_user(&a.user), a.user.is_empty());
                ui.end_row();
                ui.label("Mot de passe");
                ui.add(
                    egui::TextEdit::singleline(&mut a.password)
                        .password(true)
                        .desired_width(200.0),
                );
                err(
                    ui,
                    account::check_password(&a.password, &a.password),
                    a.password.is_empty(),
                );
                ui.end_row();
                ui.label("Confirmation");
                ui.add(
                    egui::TextEdit::singleline(&mut a.confirm)
                        .password(true)
                        .desired_width(200.0),
                );
                err(
                    ui,
                    account::check_password(&a.password, &a.confirm),
                    a.confirm.is_empty(),
                );
                ui.end_row();
                ui.label("Nom de la machine");
                ui.add(egui::TextEdit::singleline(&mut a.hostname).desired_width(200.0));
                err(ui, account::check_hostname(&a.hostname), false);
                ui.end_row();

                ui.label("Pays / Langue");
                let loc_label = account::LOCALES
                    .iter()
                    .find(|l| l.locale == a.locale)
                    .map(|l| l.label)
                    .unwrap_or(a.locale.as_str());
                egui::ComboBox::from_id_salt("locale")
                    .width(200.0)
                    .selected_text(loc_label)
                    .show_ui(ui, |ui| {
                        for preset in account::LOCALES {
                            if ui.selectable_label(a.locale == preset.locale, preset.label).clicked() {
                                a.apply_locale(preset);
                            }
                        }
                    });
                ui.label("");
                ui.end_row();

                ui.label("Clavier");
                let kb_label = account::KEYBOARDS
                    .iter()
                    .find(|k| k.xkb_layout == a.xkb_layout && k.xkb_variant == a.xkb_variant)
                    .map(|k| k.label)
                    .unwrap_or(a.xkb_layout.as_str());
                egui::ComboBox::from_id_salt("keyboard")
                    .width(200.0)
                    .selected_text(kb_label)
                    .show_ui(ui, |ui| {
                        for kb in account::KEYBOARDS {
                            let sel = a.xkb_layout == kb.xkb_layout && a.xkb_variant == kb.xkb_variant;
                            if ui.selectable_label(sel, kb.label).clicked() {
                                a.apply_keyboard(kb);
                            }
                        }
                    });
                ui.label("");
                ui.end_row();

                ui.label("Fuseau horaire");
                egui::ComboBox::from_id_salt("timezone")
                    .width(200.0)
                    .selected_text(a.timezone.as_str())
                    .show_ui(ui, |ui| {
                        for &tz in account::TIMEZONES {
                            if ui.selectable_label(a.timezone == tz, tz).clicked() {
                                a.timezone = tz.into();
                            }
                        }
                    });
                ui.label("");
                ui.end_row();
            });
        ui.add_space(10.0);
        ui.checkbox(
            &mut self.account.move_to_nvme,
            "Migrer automatiquement le système sur le SSD NVMe (/dev/nvme0n1) au premier démarrage",
        );
        if self.account.move_to_nvme {
            ui.weak("Le SSD NVMe sera formaté en ext4 au 1er boot ; /boot reste sur l'eMMC en secours.");
        }
        ui.add_space(16.0);
        ui.horizontal(|ui| {
            if ui.button("Retour").clicked() {
                self.step = Step::Prepare;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(self.account.is_valid(), egui::Button::new("Suivant"))
                    .clicked()
                {
                    self.step = Step::Recovery;
                }
            });
        });
    }

    fn recovery(&mut self, ui: &mut egui::Ui) {
        ui.heading("Mise en recovery");
        ui.add_space(8.0);
        ui.label("1. Débranche l'alimentation 12 V.");
        ui.label("2. Pose un cavalier entre FC REC et GND, sur le connecteur de boutons de la J202.");
        ui.label("3. Relie le port USB-C de la J202 à ce PC.");
        ui.label("4. Rebranche le 12 V.");
        ui.add_space(12.0);
        draw_board(ui);
        ui.add_space(12.0);

        let apx = self.apx.lock().unwrap().clone();
        match &apx {
            usb::Recovery::Absent => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("En attente de la carte en recovery (USB 0955:7f21)…");
                });
            }
            usb::Recovery::NeedsDriver(_) => {
                ui.label("Carte détectée, mais Windows n'a pas encore de pilote WinUSB pour elle :");
                self.driver_row(ui);
            }
            usb::Recovery::Ready(_) => {
                ui.colored_label(OK, "✔ Jetson détecté en recovery");
            }
        }
        ui.add_space(12.0);
        ui.checkbox(
            &mut self.verify_flash,
            "Vérifier le flash par relecture de l'eMMC (plus lent, +5 à 10 min)",
        );
        ui.add_space(6.0);
        let confirm_label = if self.account.move_to_nvme {
            "Je confirme l'effacement de la partition système de l'eMMC et du SSD NVMe (sauvegarde boot eMMC faite avant)"
        } else {
            "Je confirme l'effacement de la partition système de l'eMMC (sauvegarde boot eMMC faite avant)"
        };
        ui.checkbox(&mut self.confirm_erase, confirm_label);
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            if ui.button("Retour").clicked() {
                self.step = Step::Account;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let ok = matches!(apx, usb::Recovery::Ready(_)) && self.confirm_erase;
                if ui.add_enabled(ok, egui::Button::new("Installer")).clicked()
                    && let Some(r) = self.release()
                {
                    match self.account.firstboot_conf() {
                        Ok(conf) => {
                            let plan = install::Plan {
                                files: self.release_files(&r),
                                l4t: self.l4t(&r).dest,
                                cache: self.cache.clone(),
                                release: r,
                                firstboot_conf: conf,
                                verify_flash: self.verify_flash,
                            };
                            self.run = Some(install::start(plan, ui.ctx().clone()));
                            self.step = Step::Install;
                        }
                        Err(e) => self.driver_msg = Some(Err(e)),
                    }
                }
            });
        });
    }

    fn install(&mut self, ui: &mut egui::Ui) {
        ui.heading("Installation");
        ui.add_space(8.0);
        let Some(run) = &self.run else { return };
        let s = run.state.lock().unwrap();
        for (name, st) in install::STEPS.iter().zip(&s.steps) {
            ui.horizontal(|ui| {
                match st {
                    install::State::Waiting => ui.weak(format!("○  {name}")),
                    install::State::Running => {
                        ui.spinner();
                        ui.label(*name)
                    }
                    install::State::Done => ui.colored_label(OK, format!("✔  {name}")),
                    install::State::Skipped => ui.weak(format!("–  {name} (ignorée)")),
                    install::State::Failed => ui.colored_label(ERR, format!("✖  {name}")),
                };
            });
        }
        let finished = s.finished.clone();
        if let Some((done, total)) = s.progress {
            ui.add_space(8.0);
            let frac = if total > 0 { done as f32 / total as f32 } else { 0.0 };
            ui.add(egui::ProgressBar::new(frac).text(format!("{} / {}", mib(done), mib(total))));
        }
        ui.add_space(12.0);
        if let Some(Err(e)) = &finished {
            ui.colored_label(ERR, e);
        }
        if self.show_log {
            ui.add_space(8.0);
            egui::ScrollArea::vertical()
                .max_height(140.0)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for l in &s.log {
                        ui.monospace(l);
                    }
                });
        }
        drop(s);
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            if ui
                .button(if self.show_log { "Masquer le journal" } else { "Journal" })
                .clicked()
            {
                self.show_log = !self.show_log;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| match finished {
                None => {
                    if ui.button("Annuler").clicked()
                        && let Some(r) = &self.run
                    {
                        r.cancel();
                    }
                }
                Some(Ok(())) => {
                    if ui.button("Suivant").clicked() {
                        self.step = Step::Done;
                    }
                }
                Some(Err(_)) => {
                    if ui.button("Retour").clicked() {
                        self.run = None;
                        self.step = Step::Recovery;
                    }
                }
            });
        });
    }

    fn done(&mut self, ui: &mut egui::Ui) {
        ui.heading("Terminé");
        ui.add_space(8.0);
        ui.label("1. Débranche le 12 V et retire le cavalier FC REC.");
        ui.label("2. Rebranche le 12 V : la carte démarre sur le système installé.");
        ui.label(format!("3. Connecte-toi avec le compte {}.", self.account.user));
    }
}

/// The carrier seen from above, the jumper and the USB-C port: where to act.
fn draw_board(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(360.0, 120.0), egui::Sense::hover());
    let p = ui.painter();
    let line = ui.visuals().widgets.noninteractive.fg_stroke;
    let text = ui.visuals().text_color();
    let font = egui::FontId::proportional(13.0);
    p.rect_stroke(rect, 8.0, line, egui::StrokeKind::Inside);
    p.text(
        rect.left_top() + egui::vec2(12.0, 10.0),
        egui::Align2::LEFT_TOP,
        "J202",
        font.clone(),
        text,
    );
    let usb = egui::Rect::from_min_size(rect.left_bottom() + egui::vec2(24.0, -34.0), egui::vec2(36.0, 16.0));
    p.rect_stroke(usb, 6.0, line, egui::StrokeKind::Inside);
    p.text(
        usb.center_bottom() + egui::vec2(0.0, 4.0),
        egui::Align2::CENTER_TOP,
        "USB-C",
        font.clone(),
        text,
    );
    let pin = |x: f32| egui::pos2(rect.right() - x, rect.top() + 52.0);
    let (rec, gnd) = (pin(110.0), pin(80.0));
    p.rect_filled(
        egui::Rect::from_two_pos(rec + egui::vec2(-8.0, -8.0), gnd + egui::vec2(8.0, 8.0)),
        3.0,
        WARN,
    );
    for (pos, name) in [(rec, "FC REC"), (gnd, "GND")] {
        p.circle_filled(pos, 4.0, text);
        p.text(
            pos + egui::vec2(0.0, 14.0),
            egui::Align2::CENTER_TOP,
            name,
            font.clone(),
            text,
        );
    }
    p.text(
        pin(95.0) + egui::vec2(0.0, -14.0),
        egui::Align2::CENTER_BOTTOM,
        "cavalier",
        font,
        WARN,
    );
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // resumed after the UAC prompt to install the driver, now with admin rights
        if self.resume_driver {
            self.resume_driver = false;
            self.install_driver();
        }
        egui::Panel::left("steps")
            .exact_size(170.0)
            .resizable(false)
            .show(ui, |ui| self.sidebar(ui));
        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space(8.0);
            match self.step {
                Step::Prepare => self.prepare(ui),
                Step::Account => self.account(ui),
                Step::Recovery => self.recovery(ui),
                Step::Install => self.install(ui),
                Step::Done => self.done(ui),
            }
        });
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Installeur J202 — Jetson Nano")
            .with_inner_size([780.0, 520.0])
            .with_min_inner_size([700.0, 460.0]),
        ..Default::default()
    };
    eframe::run_native("j202-installer", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}
