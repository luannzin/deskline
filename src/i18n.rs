//! English and Brazilian Portuguese, picked from the system language. English is the source:
//! `t("Connect")` is the Portuguese line on a Portuguese system and the English one otherwise,
//! or for any line without a translation. Messages that arrive from the other computer are in
//! English too, so the window translates them the same way when it shows them; ones that end
//! in a detail ("Couldn't send x: why") are matched by their start.
use std::sync::OnceLock;

fn portuguese() -> bool {
    static PT: OnceLock<bool> = OnceLock::new();
    *PT.get_or_init(|| {
        let lang = std::env::var("DESKLINE_LANG").ok().or_else(sys_locale::get_locale).unwrap_or_default();
        lang.to_lowercase().starts_with("pt")
    })
}

/// The line in this computer's language.
pub fn t(en: &str) -> String {
    if !portuguese() {
        return en.to_string();
    }
    if let Some((_, pt)) = LINES.iter().find(|(e, _)| *e == en) {
        return pt.to_string();
    }
    match STARTS.iter().find(|(e, _)| en.starts_with(e)) {
        Some((e, pt)) => format!("{pt}{}", &en[e.len()..]),
        None => en.to_string(),
    }
}

/// `t` with each `{}` filled in order.
pub fn tf(en: &str, args: &[&str]) -> String {
    args.iter().fold(t(en), |s, a| s.replacen("{}", a, 1))
}

const LINES: &[(&str, &str)] = &[
    // window
    ("Home", "Início"),
    ("Settings", "Configurações"),
    ("Connecting…", "Conectando…"),
    ("Your code", "Seu código"),
    ("Send it to whoever is helping you. It never changes.", "Mande pra quem está te ajudando. Ele nunca muda."),
    ("Copy code", "Copiar código"),
    ("Copied", "Copiado"),
    ("Unattended access is on", "Acesso não supervisionado ligado"),
    ("Unattended access is off", "Acesso não supervisionado desligado"),
    ("Change", "Alterar"),
    ("Connect to a computer", "Conectar a um computador"),
    ("Paste their code", "Cole o código da pessoa"),
    ("Connect", "Conectar"),
    ("Their unattended-access password", "Senha de acesso não supervisionado"),
    ("Log in without the password next time", "Entrar sem a senha da próxima vez"),
    ("Recent", "Recentes"),
    ("Rename", "Renomear"),
    ("Forget", "Esquecer"),
    ("{} is no longer in Recent.", "{} não está mais nos Recentes."),
    ("Undo", "Desfazer"),
    ("Save", "Salvar"),
    ("Deskline {} is ready.", "O Deskline {} está pronto."),
    ("Restart to update", "Reiniciar pra atualizar"),
    ("Updating…", "Atualizando…"),
    ("That code doesn't look right. It's 52 letters and digits.", "Esse código não parece certo. São 52 letras e números."),
    ("That's this computer's code. Paste your partner's.", "Esse é o código deste computador. Cole o da outra pessoa."),
    ("Installing… confirm the Windows prompt.", "Instalando… confirme o aviso do Windows."),
    ("Saved. Restart Deskline to use it.", "Salvo. Reinicie o Deskline pra usar."),
    ("Or log in with their unattended-access password", "Ou entre com a senha de acesso não supervisionado"),
    ("Log in", "Entrar"),
    ("Cancel", "Cancelar"),
    ("Ask for control", "Pedir controle"),
    ("Waiting for them to allow control…", "Esperando a pessoa liberar o controle…"),
    ("You have control", "Você tem o controle"),
    ("Privacy", "Privacidade"),
    ("Blank their screen and lock their own mouse and keyboard", "Apaga a tela da pessoa e trava o mouse e o teclado dela"),
    ("Screen", "Tela"),
    ("Picture", "Imagem"),
    ("Balanced", "Equilibrada"),
    ("Sharp text", "Texto nítido"),
    ("Slow connection", "Conexão lenta"),
    ("Sound", "Som"),
    ("Actual size", "Tamanho real"),
    ("Full screen", "Tela cheia"),
    ("F11 to leave", "F11 pra sair"),
    ("Files", "Arquivos"),
    ("Chat", "Chat"),
    ("Disconnect", "Desconectar"),
    ("They said no to control.", "A pessoa não liberou o controle."),
    ("Waiting for the first picture…", "Esperando a primeira imagem…"),
    ("File transfer needs control. Ask for control first.", "Transferir arquivos precisa do controle. Peça o controle primeiro."),
    ("Send files…", "Enviar arquivos…"),
    ("Send a folder…", "Enviar uma pasta…"),
    (
        "Lands in their Downloads. You can also drop files on the window.",
        "Chega nos Downloads da pessoa. Você também pode arrastar arquivos pra janela.",
    ),
    ("Up", "Voltar"),
    ("Their computer", "Computador da pessoa"),
    ("This folder is empty.", "Esta pasta está vazia."),
    ("Download", "Baixar"),
    ("Into your Downloads", "Pros seus Downloads"),
    ("Message", "Mensagem"),
    ("Send", "Enviar"),
    ("You", "Você"),
    ("Them", "Outro lado"),
    ("{} sees your screen", "{} está vendo sua tela"),
    ("{} sees and controls your screen", "{} está vendo e controlando sua tela"),
    ("Take back control", "Retomar o controle"),
    ("Give control", "Dar o controle"),
    ("Stop sharing", "Parar de compartilhar"),
    ("Chat with {}", "Chat com {}"),
    ("Someone wants to see your screen", "Alguém quer ver sua tela"),
    ("Only accept if you know who this is.", "Só aceite se você souber quem é."),
    ("Let them control (mouse, keyboard, clipboard, files)", "Deixar controlar (mouse, teclado, área de transferência, arquivos)"),
    ("Always allow them, even when I'm away", "Sempre permitir, mesmo quando eu não estiver aqui"),
    ("Accept", "Aceitar"),
    ("Decline", "Recusar"),
    ("Control request", "Pedido de controle"),
    ("{} wants to control your mouse and keyboard.", "{} quer controlar seu mouse e teclado."),
    ("You can take control back at any time from the red bar.", "Você pode retomar o controle quando quiser pela barra vermelha."),
    ("Allow", "Permitir"),
    ("Deny", "Negar"),
    ("Unattended access", "Acesso não supervisionado"),
    (
        "With a password, people who know it can connect and control this computer when nobody is here to click Accept.",
        "Com uma senha, quem souber pode se conectar e controlar este computador quando não tiver ninguém aqui pra clicar em Aceitar.",
    ),
    ("A password is set.", "Tem uma senha definida."),
    ("No password is set.", "Nenhuma senha definida."),
    ("A password is set. Set a new one to replace it.", "Tem uma senha definida. Defina outra pra trocar."),
    ("Change as administrator", "Alterar como administrador"),
    ("New password (8 characters or more)", "Nova senha (8 caracteres ou mais)"),
    ("Repeat it", "Repita a senha"),
    ("Set password", "Definir senha"),
    ("Turn off", "Desligar"),
    ("Use at least 8 characters.", "Use pelo menos 8 caracteres."),
    ("The two passwords are different.", "As duas senhas estão diferentes."),
    ("Password set.", "Senha definida."),
    ("Unattended access is off.", "Acesso não supervisionado desligado."),
    ("Always allowed", "Sempre permitidos"),
    ("Remove", "Remover"),
    ("Sessions", "Sessões"),
    ("When I accept someone, let them control by default", "Quando eu aceitar alguém, deixar controlar por padrão"),
    ("Hear the other computer's sound", "Ouvir o som do outro computador"),
    ("This computer", "Este computador"),
    ("Start Deskline when I sign in", "Abrir o Deskline quando eu entrar no computador"),
    (
        "Installed: Deskline starts with Windows and works at the sign-in screen.",
        "Instalado: o Deskline inicia com o Windows e funciona na tela de login.",
    ),
    ("Uninstall", "Desinstalar"),
    ("Install for all users", "Instalar para todos os usuários"),
    ("Starts with Windows and works at the sign-in screen.", "Inicia com o Windows e funciona na tela de login."),
    ("Relay server", "Servidor de relay"),
    (
        "Leave empty for the public relays. Your own: run iroh-relay and paste its URL. Restart Deskline after changing.",
        "Deixe vazio pra usar os relays públicos. Pra usar o seu: rode o iroh-relay e cole a URL. Reinicie o Deskline depois de mudar.",
    ),
    ("Open session log", "Abrir o registro de sessões"),
    (
        "Session log: %ProgramData%\\Deskline\\sessions.log (administrators)",
        "Registro de sessões: %ProgramData%\\Deskline\\sessions.log (administradores)",
    ),
    ("Open Deskline", "Abrir o Deskline"),
    ("Quit", "Sair"),
    ("Direct", "Direto"),
    // from the session and the other computer
    ("They declined.", "A pessoa recusou."),
    ("Wrong password.", "Senha errada."),
    ("Nobody answered.", "Ninguém respondeu."),
    ("Too many wrong passwords. Try again in a few minutes.", "Muitas senhas erradas. Tente de novo em alguns minutos."),
    (
        "Nobody is at that computer to accept. Use its unattended-access password.",
        "Não tem ninguém naquele computador pra aceitar. Use a senha de acesso não supervisionado.",
    ),
    (
        "Nobody is at that computer to accept, and it has no unattended-access password.",
        "Não tem ninguém naquele computador pra aceitar, e ele não tem senha de acesso não supervisionado.",
    ),
    ("That computer has too many sessions open.", "Aquele computador está com sessões demais abertas."),
    ("They disconnected.", "A pessoa desconectou."),
    ("Connection lost.", "A conexão caiu."),
    ("You stopped sharing.", "Você parou de compartilhar."),
    ("They stopped sharing.", "A pessoa parou de compartilhar."),
    ("The session ended.", "A sessão terminou."),
    ("Reaching their computer…", "Procurando o computador…"),
    ("Waiting for them to accept…", "Esperando a pessoa aceitar…"),
    ("Connection lost. Reconnecting…", "A conexão caiu. Reconectando…"),
    ("Couldn't reach that computer. Is Deskline open there?", "Não deu pra achar aquele computador. O Deskline está aberto lá?"),
    ("That computer runs an older Deskline. Ask them to update it.", "Aquele computador usa um Deskline antigo. Peça pra atualizar."),
    ("They closed the connection.", "A pessoa fechou a conexão."),
    ("Reconnected.", "Reconectado."),
    ("Sharing stopped.", "O compartilhamento parou."),
    ("Privacy mode isn't available on Linux.", "O modo privacidade não existe no Linux."),
    ("Privacy mode isn't available on macOS.", "O modo privacidade não existe no macOS."),
    ("Unexpected reply.", "Resposta inesperada."),
    // games, and choosing again what's shared
    ("Gaming (120 fps)", "Jogos (120 fps)"),
    ("Other screens…", "Outras telas…"),
    ("Asks their computer to choose again what it shares", "Pede ao computador da pessoa pra escolher de novo o que compartilha"),
    ("Asked them to choose again what they share.", "Pedimos pra pessoa escolher de novo o que compartilha."),
    ("Capture mouse", "Capturar mouse"),
    (
        "For games: your mouse moves theirs directly. Ctrl+Alt+Shift+Z gives it back.",
        "Pra jogos: seu mouse move o da pessoa direto. Ctrl+Alt+Shift+Z devolve.",
    ),
    ("Ctrl+Alt+Shift+Z gives the mouse back", "Ctrl+Alt+Shift+Z devolve o mouse"),
    ("Choose screens", "Escolher telas"),
    ("Pick again what they see, and whether they may control", "Escolha de novo o que a pessoa vê e se ela pode controlar"),
    ("They're already choosing what to share.", "A pessoa já está escolhendo o que compartilhar."),
    (
        "They closed their sharing dialog a moment ago. Ask them on the phone or in chat.",
        "A pessoa fechou a janela de compartilhamento agora há pouco. Peça por telefone ou no chat.",
    ),
    ("All of their screens are already in the list.", "Todas as telas da pessoa já estão na lista."),
    (
        "They didn't allow remote control in their system's sharing dialog.",
        "A pessoa não permitiu o controle remoto na janela de compartilhamento do sistema.",
    ),
];

/// Lines that end in a detail (a name, a path, an error), longest starts first.
const STARTS: &[(&str, &str)] = &[
    ("Couldn't reach that computer: ", "Não deu pra achar aquele computador: "),
    ("Their screen couldn't be shared: ", "A tela da pessoa não pôde ser compartilhada: "),
    ("Couldn't share your screen: ", "Não deu pra compartilhar sua tela: "),
    ("Couldn't open that folder: ", "Não deu pra abrir essa pasta: "),
    ("Couldn't switch screens: ", "Não deu pra trocar de tela: "),
    ("No sound from this computer: ", "Sem som deste computador: "),
    ("Can't play their sound: ", "Não dá pra tocar o som da pessoa: "),
    ("Couldn't change that: ", "Não deu pra mudar isso: "),
    ("Couldn't download ", "Não deu pra baixar "),
    ("Couldn't send ", "Não deu pra enviar "),
    ("Update failed: ", "A atualização falhou: "),
    ("Connection lost: ", "A conexão caiu: "),
    ("Sharing stopped: ", "O compartilhamento parou: "),
    ("Sent to ", "Enviado para "),
    ("Saved ", "Salvo em "),
    ("Received ", "Recebido: "),
    ("Sent ", "Enviado: "),
    ("File transfer failed: ", "A transferência falhou: "),
    ("Nothing changed on their side: ", "Nada mudou do lado da pessoa: "),
    ("Their computer refused the mouse or keyboard: ", "O computador da pessoa recusou o mouse ou o teclado: "),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_line_keeps_its_blanks() {
        for (en, pt) in LINES {
            assert_eq!(en.matches("{}").count(), pt.matches("{}").count(), "{en}");
        }
        for (i, (a, _)) in STARTS.iter().enumerate() {
            // a shorter start listed earlier would shadow this one
            assert!(!STARTS[..i].iter().any(|(b, _)| a.starts_with(b)), "{a}");
        }
    }
}
