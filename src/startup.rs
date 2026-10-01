//! Instrumentation du démarrage : modèle d'étapes commun aux deux
//! modes de restitution.
//!
//! - `opencode-start-menu -v|--verbose` : trace textuelle sur stderr.
//! - `opencode-start-menu` : jauge animée, pilotée par les mêmes événements.
//!
//! Le travail de démarrage est exécuté dans un thread qui émet des `Event`
//! sur un canal ; le thread principal les consomme pour animer la jauge.
//! Le mode verbose consomme exactement les mêmes événements, en printing.

use std::{
    sync::mpsc,
    time::{Duration, Instant},
};

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::App;

// ---------------------------------------------------------------------------
// Étapes
// ---------------------------------------------------------------------------

/// Une étape du démarrage, dans l'ordre d'exécution garanti.
///
/// Les quatorze étapes listées dans [`ALL_STEPS`] sont celles exécutées par le
/// thread de démarrage : ce sont les seules supervisées par la jauge.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Step
  {
    Args,
    WhichOpencode,
    Paths,
    WebCache,
    Registry,
    RescanPurge,
    RescanIndex,
    RescanUsage,
    RescanTarifs,
    RescanManifests,
    AutoPatch,
    RescanSave,
    SyncVersion,
    Finalize,
  }

impl Step
  {
    /// Libellé français affiché dans la jauge et dans la trace.
    pub(crate) fn label(self) -> &'static str
      {
        match self
          {
            Step::Args => "Analyse des arguments",
            Step::WhichOpencode => "Vérification de opencode dans le PATH",
            Step::Paths => "Résolution des chemins (registre, base SQLite)",
            Step::WebCache => "Chargement du cache des tarifs",
            Step::Registry => "Chargement du registre des projets",
            Step::RescanPurge => "Rescan — purge des entrées obsolètes",
            Step::RescanIndex => "Rescan — indexation depuis le disque",
            Step::RescanUsage => "Rescan — lecture de l'usage SQLite",
            Step::RescanTarifs => "Rescan — rafraîchissement des tarifs web",
            Step::RescanManifests => "Rescan — manifestes, variantes et usage",
            Step::AutoPatch => "AutoPatch — hash des sources et bump de version",
            Step::RescanSave => "Rescan — écriture du registre",
            Step::SyncVersion => "Synchronisation de la version du menu",
            Step::Finalize => "Finalisation et tri de la liste",
          }
      }

    /// Rang 1-based dans [`ALL_STEPS`].
    pub(crate) fn index(self) -> usize
      {
        ALL_STEPS.iter().position(|s| *s == self).map_or(0, |i| i + 1)
      }
}

/// Ordre d'exécution et liste de référence de la jauge.
pub(crate) const ALL_STEPS: [Step; 14] = [
  Step::Args,
  Step::WhichOpencode,
  Step::Paths,
  Step::WebCache,
  Step::Registry,
  Step::RescanPurge,
  Step::RescanIndex,
  Step::RescanUsage,
  Step::RescanTarifs,
  Step::RescanManifests,
  Step::AutoPatch,
  Step::RescanSave,
  Step::SyncVersion,
  Step::Finalize,
];

// ---------------------------------------------------------------------------
// Événements
// ---------------------------------------------------------------------------

/// Transition d'étape transmise du thread de démarrage au thread principal.
#[derive(Debug)]
pub(crate) enum Event
  {
    Begin { step: Step, detail: Option<String> },
    Detail { step: Step, detail: String },
    Done { step: Step, took: Duration, detail: Option<String> },
    Skipped { step: Step },
}

/// Ce que le thread de démarrage renvoie au thread principal : soit des
/// transitions d'étape, soit l'`App` prête à être affichée.
pub(crate) enum Signal
  {
    Step(Event),
    Ready(Box<App>),
}

/// Destination des transitions d'étape.
pub(crate) trait Reporter
  {
    fn emit(&self, event: Event);
  }

/// Reporter qui écrit une ligne par transition sur stderr.
pub(crate) struct Verbose
  {
    start: Instant,
  }

impl Verbose
  {
    pub(crate) fn new() -> Self
      {
        Self { start: Instant::now() }
      }

    pub(crate) fn note(&self, mark: char, label: &str, extra: &str)
      {
        let ms = self.start.elapsed().as_millis();
        if extra.is_empty()
          {
            eprintln!("[{:>8} ms] {} {}", ms, mark, label);
          }
        else
          {
            eprintln!("[{:>8} ms] {} {} — {}", ms, mark, label, extra);
          }
      }
  }

impl Reporter for Verbose
  {
    fn emit(&self, event: Event)
      {
        match event
          {
            Event::Begin { step, detail } => {
              self.note('▶', &format!("{:02} · {}", step.index(), step.label()), detail.as_deref().unwrap_or(""));
            }
            Event::Detail { .. } => {
              // Le sous-detail est déjà visible sur la ligne Begin/Done suivante ;
              // en verbose on ne le répète pas à chaque projet.
            }
            Event::Done { step, took, detail } => {
              let extra = format!("{} ms{}", took.as_millis(), suffix(detail.as_deref()));
              self.note('✓', &format!("{:02} · {}", step.index(), step.label()), &extra);
            }
            Event::Skipped { step } => {
              self.note('—', &format!("{:02} · {}", step.index(), step.label()), "ignorée");
            }
          }
      }
}

fn suffix(detail: Option<&str>) -> String
  {
    match detail
      {
        Some(d) if !d.is_empty() => format!(" · {d}"),
        _ => String::new(),
      }
  }

/// Reporter qui transmet au thread principal.
pub(crate) struct Channel(pub(crate) mpsc::Sender<Signal>);

impl Reporter for Channel
  {
    fn emit(&self, event: Event)
      {
        let _ = self.0.send(Signal::Step(event));
      }
  }

/// Reporter inerte, pour les chemins non instrumentés (TUI, tests).
pub(crate) struct Noop;

impl Reporter for Noop
  {
    fn emit(&self, _event: Event) {}
  }

static NOOP: Noop = Noop;

// ---------------------------------------------------------------------------
// Chronomètre
// ---------------------------------------------------------------------------

/// Chronomètre d'étapes : ferme automatiquement l'étape courante à chaque
/// `begin` sur la suivante, pour qu'aucun chemin d'erreur n'oublie un `done`.
pub(crate) struct Progress<'a>
  {
    reporter: &'a dyn Reporter,
    current: Option<(Step, Instant, Option<String>)>,
  }

impl<'a> Progress<'a>
  {
    pub(crate) fn new(reporter: &'a dyn Reporter) -> Self
      {
        Self { reporter, current: None }
      }

    /// Chronomètre muet, pour les appels hors démarrage (rescan depuis la
    /// TUI, tests unitaires).
    pub(crate) fn noop() -> Progress<'static>
      {
        Progress { reporter: &NOOP, current: None }
      }

    pub(crate) fn begin(&mut self, step: Step)
      {
        self.done();
        self.current = Some((step, Instant::now(), None));
        self.reporter.emit(Event::Begin { step, detail: None });
      }

    /// Attache un sous-detail à l'étape courante et le publie aussitôt, pour
    /// que la jauge affiche l'avancement interne (projet N/M).
    pub(crate) fn detail(&mut self, step: Step, detail: impl Into<String>)
      {
        let detail = detail.into();
        if let Some((s, _, d)) = self.current.as_mut()
          && *s == step
          {
            *d = Some(detail.clone());
          }
        self.reporter.emit(Event::Detail { step, detail });
      }

    /// Ferme l'étape courante, si elle existe.
    pub(crate) fn done(&mut self)
      {
        if let Some((step, since, detail)) = self.current.take()
          {
            self.reporter.emit(Event::Done { step, took: since.elapsed(), detail });
          }
      }

    /// Ferme une étape dont la durée a été mesurée séparément, sans toucher à
    /// l'étape courante. Utilisé quand une seule boucle par projet couvre
    /// deux étapes distinctes, et pour les étapes ouvertes a posteriori.
    pub(crate) fn close(&mut self, step: Step, took: Duration, detail: impl Into<String>)
      {
        let detail = detail.into();
        if let Some((s, _, _)) = self.current.as_ref()
          && *s == step
          {
            self.current = None;
          }
        self.reporter.emit(Event::Begin { step, detail: None });
        self.reporter.emit(Event::Done { step, took, detail: Some(detail) });
      }

    /// Signale une étape non exécutée. Ferme l'étape courante si c'était elle :
    /// sans cela, le `begin` suivant réémettrait un `Done` sur une étape déjà
    /// déclarée ignorée, et la trace afficherait deux lignes pour elle.
    pub(crate) fn skip(&mut self, step: Step)
      {
        if let Some((s, _, _)) = self.current.as_ref()
          && *s == step
          {
            self.current = None;
          }
        self.reporter.emit(Event::Skipped { step });
      }
}

// ---------------------------------------------------------------------------
// État de la jauge (thread principal)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum Status
  {
    Pending,
    Running,
    Done(Duration),
    Skipped,
  }

struct Entry
  {
    step: Step,
    status: Status,
    detail: String,
}

/// Instantané des étapes, reconstruit à chaque image à partir du canal.
pub(crate) struct GaugeState
  {
    start: Instant,
    frame: u64,
    entries: Vec<Entry>,
  }

impl GaugeState
  {
    pub(crate) fn new() -> Self
      {
        Self {
          start: Instant::now(),
          frame: 0,
          entries: ALL_STEPS
            .iter()
            .map(|step| Entry { step: *step, status: Status::Pending, detail: String::new() })
            .collect(),
        }
      }

    pub(crate) fn tick(&mut self)
      {
        self.frame = self.frame.wrapping_add(1);
      }

    pub(crate) fn apply(&mut self, event: Event)
      {
        match event
          {
            Event::Begin { step, .. } => {
              if let Some(e) = self.find(step)
                {
                  e.status = Status::Running;
                  e.detail.clear();
                }
            }
            Event::Detail { step, detail } => {
              if let Some(e) = self.find(step)
                {
                  e.detail = detail;
                }
            }
            Event::Done { step, took, detail } => {
              if let Some(e) = self.find(step)
                {
                  e.status = Status::Done(took);
                  e.detail = detail.unwrap_or_else(|| e.detail.clone());
                }
            }
            Event::Skipped { step } => {
              if let Some(e) = self.find(step)
                {
                  e.status = Status::Skipped;
                  e.detail.clear();
                }
            }
          }
      }

    fn find(&mut self, step: Step) -> Option<&mut Entry>
      {
        self.entries.iter_mut().find(|e| e.step == step)
      }
}

// ---------------------------------------------------------------------------
// Rendu
// ---------------------------------------------------------------------------

fn human(d: Duration) -> String
  {
    let ms = d.as_millis();
    if ms < 1_000
      {
        format!("{ms} ms")
      }
    else
      {
        format!("{:.2} s", d.as_secs_f64())
      }
  }

fn dot(status: &Status) -> (&'static str, Color)
  {
    match status
      {
        Status::Pending => ("·", Color::DarkGray),
        Status::Running => ("▶", Color::Yellow),
        Status::Done(_) => ("✓", Color::Green),
        Status::Skipped => ("—", Color::DarkGray),
      }
  }

/// Barre de `width` cellules. Une étape terminée est proportionnelle à sa
/// propre durée ; l'étape en cours reçoit une barre animée qui pulse.
fn bar(status: &Status, frame: u64, ratio_to_max: f64, width: usize) -> Vec<Span<'static>>
  {
    let filled = match status
      {
        Status::Pending | Status::Skipped => 0.0,
        Status::Running => {
          let phase = (frame % 18) as f64 / 18.0 * std::f64::consts::TAU;
          0.10 + 0.80 * (0.5 + 0.5 * phase.cos())
        }
        Status::Done(_) => ratio_to_max.clamp(0.04, 1.0),
      };
    let n = (filled * width as f64).round().clamp(0.0, width as f64) as usize;
    let color = match status
      {
        Status::Running => Color::Yellow,
        Status::Done(_) => Color::Green,
        Status::Pending => Color::DarkGray,
        Status::Skipped => Color::DarkGray,
      };
    vec![
      Span::styled("█".repeat(n), Style::default().fg(color)),
      Span::styled("░".repeat(width - n), Style::default().fg(Color::DarkGray)),
    ]
  }

pub(crate) fn render(f: &mut Frame, state: &GaugeState)
  {
    let area = f.area();
    let chunks = Layout::default()
      .direction(Direction::Vertical)
      .constraints([
      Constraint::Length(3),
      Constraint::Length(1),
      Constraint::Min(0),
    ])
      .split(area);

    // Bandeau : libellé, avancement, temps écoulé.
    let done = state
      .entries
      .iter()
      .filter(|e| !matches!(e.status, Status::Pending | Status::Running))
      .count();
    let header = Block::default()
      .borders(Borders::ALL)
      .title(" OpenCode Start Menu — initialisation ")
      .title_alignment(Alignment::Center);
    let total = format!(
      " {done}/{} · {} ",
      ALL_STEPS.len(),
      human(state.start.elapsed())
    );
    f.render_widget(Paragraph::new(total).block(header).alignment(Alignment::Right), chunks[0]);

    // Série de points : une pastille par étape, dans l'ordre.
    let dots: Vec<Span> = state
      .entries
      .iter()
      .map(|e| {
        let (mark, color) = dot(&e.status);
        Span::styled(format!("{mark} "), Style::default().fg(color))
      })
      .collect();
    f.render_widget(Paragraph::new(Line::from(dots)), chunks[1]);

    // Une ligne par étape : pastille, rang, libellé, barre propre, durée.
    let max_done = state
      .entries
      .iter()
      .filter_map(|e| match e.status
        {
          Status::Done(d) => Some(d.as_secs_f64()),
          _ => None,
        })
      .fold(0.0_f64, f64::max);

    let lines: Vec<Line> = state
      .entries
      .iter()
      .map(|e| {
        let (mark, color) = dot(&e.status);
        let ratio = match e.status
          {
            Status::Done(d) if max_done > 0.0 => d.as_secs_f64() / max_done,
            _ => 0.0,
          };
        let dur = match e.status
          {
            Status::Done(d) => human(d),
            Status::Running => format!("{}…", human(state.start.elapsed())),
            _ => String::new(),
          };
        let head = format!(" {} {:02} · {:<44} ", mark, e.step.index(), e.step.label());
        let mut spans = vec![
          Span::styled(head, Style::default().fg(color)),
        ];
        spans.extend(bar(&e.status, state.frame, ratio, 14));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(format!("{dur:>9}"), Style::default().fg(Color::Gray)));
        if !e.detail.is_empty() && matches!(e.status, Status::Running)
          {
            spans.push(Span::styled(format!("  {}", e.detail), Style::default().fg(Color::DarkGray)));
          }
        Line::from(spans)
      })
      .collect();

    f.render_widget(Paragraph::new(lines), chunks[2]);
}

// ---------------------------------------------------------------------------
// Détail de l'étape courante pour le mode verbose (terminal)
// ---------------------------------------------------------------------------

/// Ligne de trace finale, émise hors jauge : le contrôle du wrapper et la
/// mise en route du terminal ont lieu après la fermeture de la jauge, en mode
/// cooked, parce que le premier lit une réponse sur stdin.
pub(crate) fn trace_tail(verbose: &Verbose, took: Duration, ok: bool)
  {
    verbose.note(
      if ok { '✓' } else { '—' },
      "Vérification du wrapper opencode-wrap",
      &format!("{} ms", took.as_millis()),
    );
  }

pub(crate) fn trace_terminal(verbose: &Verbose, took: Duration)
  {
    verbose.note('✓', "Initialisation du terminal", &format!("{} ms", took.as_millis()));
  }

#[cfg(test)]
mod tests
  {
    use super::*;

    /// Reporter de test qui conserve l'ordre des transitions.
    struct Collecteur(std::cell::RefCell<Vec<Event>>);

    impl Reporter for Collecteur
      {
        fn emit(&self, event: Event)
          {
            self.0.borrow_mut().push(event);
          }
      }

    fn journal(progress: &mut Progress, f: impl FnOnce(&mut Progress))
      {
        f(progress);
        progress.done();
      }

    #[test]
    fn begin_ferme_l_etape_precedente()
      {
        let c = Collecteur(std::cell::RefCell::new(Vec::new()));
        let mut p = Progress::new(&c);
        p.begin(Step::Args);
        p.begin(Step::Paths);
        p.done();
        let e = c.0.borrow();
        // Args n'est jamais « done » explicitement : c'est `begin` qui le ferme.
        let fermes: Vec<Step> = e
          .iter()
          .filter_map(|ev| match ev
            {
              Event::Done { step, .. } => Some(*step),
              _ => None,
            })
          .collect();
        assert_eq!(fermes, vec![Step::Args, Step::Paths]);
      }

    #[test]
    fn skip_emet_puis_est_ignore_par_done()
      {
        let c = Collecteur(std::cell::RefCell::new(Vec::new()));
        let mut p = Progress::new(&c);
        p.begin(Step::Registry);
        p.skip(Step::RescanTarifs);
        p.done();
        assert!(
          c.0
            .borrow()
            .iter()
            .any(|ev| matches!(ev, Event::Skipped { step: Step::RescanTarifs }))
        );
      }

    #[test]
    fn close_ne_duplique_pas_la_fermeture_de_l_etape_courante()
      {
        let c = Collecteur(std::cell::RefCell::new(Vec::new()));
        let mut p = Progress::new(&c);
        p.begin(Step::RescanManifests);
        p.close(Step::RescanManifests, Duration::from_millis(7), "3 projet(s)");
        // Un `begin` suivant ne doit pas réémettre un Done pour cette étape.
        p.begin(Step::SyncVersion);
        p.done();
        let count = c
          .0
          .borrow()
          .iter()
          .filter(|ev| matches!(ev, Event::Done { step: Step::RescanManifests, .. }))
          .count();
        assert_eq!(count, 1);
      }

    #[test]
    fn detail_est_porte_par_la_fermeture()
      {
        let c = Collecteur(std::cell::RefCell::new(Vec::new()));
        let mut p = Progress::new(&c);
        p.begin(Step::RescanManifests);
        p.detail(Step::RescanManifests, "projet 2/5");
        p.done();
        let detail = c
          .0
          .borrow()
          .iter()
          .find_map(|ev| match ev
            {
              Event::Done { detail, .. } => detail.clone(),
              _ => None,
            });
        assert_eq!(detail.as_deref(), Some("projet 2/5"));
      }

    #[test]
    fn noop_ne_produit_aucun_evenement()
      {
        journal(&mut Progress::noop(), |p| {
          p.begin(Step::Args);
          p.detail(Step::Args, "rien");
          p.skip(Step::WebCache);
        });
      }

    #[test]
    fn index_suit_la_liste_de_reference()
      {
        assert_eq!(ALL_STEPS.len(), 14);
        for (i, step) in ALL_STEPS.iter().enumerate()
          {
            assert_eq!(step.index(), i + 1);
            assert!(!step.label().is_empty());
          }
      }

    #[test]
    fn etat_de_jauge_suit_begin_puis_done()
      {
        let mut s = GaugeState::new();
        assert!(s
          .entries
          .iter()
          .all(|e| matches!(e.status, Status::Pending)));
        s.apply(Event::Begin { step: Step::Args, detail: None });
        assert!(matches!(s.entries[0].status, Status::Running));
        s.apply(Event::Done {
          step: Step::Args,
          took: Duration::from_millis(12),
          detail: Some("ok".to_string()),
        });
        match s.entries[0].status
          {
            Status::Done(d) => assert_eq!(d.as_millis(), 12),
            ref autre => panic!("attendu Done, obtenu {autre:?}"),
          }
        assert_eq!(s.entries[0].detail, "ok");
        // Les étapes suivantes restent en attente.
        assert!(matches!(s.entries[1].status, Status::Pending));
      }

    #[test]
    fn barre_animee_reste_dans_les_bornes()
      {
        for frame in 0..40u64
          {
            let filled = match &Status::Running
              {
                Status::Running =>
                  {
                    let phase = (frame % 18) as f64 / 18.0 * std::f64::consts::TAU;
                    0.10 + 0.80 * (0.5 + 0.5 * phase.cos())
                  }
                _ => unreachable!(),
              };
            assert!((0.0..=1.0).contains(&filled), "frame {frame} : {filled}");
            let n = (filled * 14.0).round() as usize;
            assert!(n <= 14);
          }
      }

    #[test]
    fn duree_lisible_sans_fractions_inutiles()
      {
        assert_eq!(human(Duration::from_millis(0)), "0 ms");
        assert_eq!(human(Duration::from_millis(999)), "999 ms");
        assert_eq!(human(Duration::from_millis(1_500)), "1.50 s");
      }
  }
