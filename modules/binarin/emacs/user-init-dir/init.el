;;; -*- mode: emacs-lisp; lexical-binding: t -*-
(require 'rx)

;;; § General
;;; §§ custom-file
(setf custom-file (locate-user-emacs-file "custom.el"))
(when (file-exists-p custom-file) (load custom-file))

;;; §§ history

(setf auto-save-list-file-prefix (locate-user-emacs-file "auto-save-dir"))
(savehist-mode t)

;; Mostly do not want to track down which sensitive files should be excluded from backups
(setf make-backup-files nil)
(setf create-lockfiles nil)

(use-package tramp
  :ensure nil
  :defines (tramp-ssh-controlmaster-options)
  :config
  (add-to-list 'tramp-remote-path 'tramp-own-remote-path)
  ;; just defer to ssh_config
  (setf tramp-ssh-controlmaster-options ""))

(use-package tramp-rpc
  :ensure nil
  :no-require t
  :defines (tramp-rpc-deploy-never-deploy)
  :config
  (setf tramp-rpc-deploy-never-deploy t))

(use-package recentf
  :ensure nil
  :defines (recentf-keep recentf-exclude)
  :init
  (setf recentf-max-saved-items 1000
        recentf-auto-cleanup 300
        recentf-show-messages nil)
  (setopt recentf-autosave-interval 180)
  :config
  ;; Keep remote/TRAMP files out of recentf — checking their
  ;; existence triggers noisy connection-attempt failures.
  (defun b/recentf-keep-p (file)
    "Return non-nil if FILE is a readable local file."
    (and (not (file-remote-p file))
         (file-readable-p file)))
  (setq recentf-keep '(b/recentf-keep-p))
  ;; Belt-and-suspenders: prevent remote files from being added.
  (add-to-list 'recentf-exclude #'file-remote-p)
  (recentf-mode t))

(save-place-mode t)
(setopt save-place-autosave-interval 180)


;;; §§ Overriding defaults

(unbind-key "C-z")
(unbind-key "C-x C-z")

(fset 'yes-or-no-p 'y-or-n-p)

(put 'set-goal-column 'disabled nil)
(put 'narrow-to-region 'disabled nil)

;;; §§ Locale
(set-language-environment "English")
(set-language-environment-input-method "Russian")

;;; §§ Outline

(use-package outline
  :ensure nil
  :commands (outline-minor-mode)
  :config
  (setq outline-minor-mode-use-buttons nil))


;;; §§ Keybindings
(which-key-mode t)
;;; § Visual

(load-theme 'batppuccin-mocha t)
(add-to-list 'default-frame-alist '(font . "IosevkaTerm Nerd Font-16"))



;;; § Completion

(setf enable-recursive-minibuffers t)

;; Hide commands in M-x which do not work in the current mode.  Vertico
;; commands are hidden in normal buffers. This setting is useful beyond
;; Vertico.
(setf read-extended-command-predicate #'command-completion-default-include-p)

;; Do not allow the cursor in the minibuffer prompt
(setf minibuffer-prompt-properties
      '(read-only t cursor-intangible t face minibuffer-prompt))

(use-package vertico
  :ensure nil
  :commands (vertico-mode)
  :init
  (vertico-mode))

(use-package vertico-directory
  :commands (vertico-directory-delete-word)
  :defines (vertico-directory-map)
  :after vertico
  :hook ((rfn-eshadow-update-overlay . vertico-directory-tidy))
  :config
  (keymap-set vertico-directory-map "C-l" #'vertico-directory-delete-word)
  :ensure nil)

(use-package vertico-multiform
  :commands (vertico-multiform-mode)
  :ensure nil
  :after vertico)

(add-hook 'rfn-eshadow-update-overlay-hook #'vertico-directory-tidy)

(setf vertico-multiform-categories
      '((file (:keymap . vertico-directory-map))))

(use-package orderless-kwd
  :ensure nil
  :commands (orderless-kwd-dispatch))

(use-package orderless
  :ensure nil
  :custom
  (completion-styles '(orderless basic))
  :config
  (add-to-list 'orderless-style-dispatchers #'orderless-kwd-dispatch))

(use-package marginalia
  :ensure t
  :bind (:map minibuffer-local-map
	      ("M-A" . marginalia-cycle))
  :commands (marginalia-mode)
  :init
  (marginalia-mode t))

(use-package corfu-history :ensure nil :after corfu :commands (corfu-history-mode))
(use-package corfu-popupinfo :ensure nil :after corfu :commands (corfu-popupinfo-mode))

(use-package corfu
  :ensure nil
  :commands (global-corfu-mode)
  :init
  (global-corfu-mode)
  (corfu-history-mode)
  (corfu-popupinfo-mode))

(use-package xref :ensure nil :defer t)

(use-package consult
  :ensure t
  :commands (consult-xref)
  ;; XXX continue binding
  :bind (([remap switch-to-buffer] . consult-buffer)
	 ([remap switch-to-buffer-other-window] . consult-buffer-other-window)
	 ([remap switch-to-buffer-other-tab] . consult-buffer-other-tab)
	 ([remap switch-to-buffer-other-frame] . consult-buffer-other-frame)
	 ([remap yank-pop] . consult-yank-pop)
         ([remap point-to-register] . consult-register-store)
         ([remap insert-register] . consult-register-load)
         ([remap goto-line] . consult-goto-line)
         ([remap repeat-complex-command] . consult-complex-command))
  :config
  (advice-add #'register-preview :override 'consult-register-window)
  (setf xref-show-xrefs-function #'consult-xref
        xref-show-definitions-function #'consult-xref
        register-preview-function 'consult-register-format
        consult-narrow-key ","))

(use-package embark
  :ensure t
  :bind (("C-;" . embark-act)
         :map embark-buffer-map
         ("F" . switch-to-buffer-other-frame)
         ;; ("f" . b/switch-to-buffer-dedicated-frame)
	 )
  :commands (embark-context-menu)
  :init
  (add-hook 'context-menu-functions #'embark-context-menu 100)
  :config
  ;; Hide the mode line of the Embark live/completions buffers
  (add-to-list 'display-buffer-alist
               '("\\`\\*Embark Collect \\(Live\\|Completions\\)\\*"
                 nil
                 (window-parameters (mode-line-format . none))))
  (add-to-list 'vertico-multiform-categories '(embark-keybinding grid)))

(use-package embark-consult
  :ensure t)

(setf tab-always-indent 'complete
      ;; TAB cycle if there are only few candidates
      completion-cycle-threshold 2

      ;; partial-completion is important for file wildcard
      ;; support. Multiple files can be opened at once with find-file if
      ;; you enter a wildcard.
      completion-category-overrides '((file (styles partial-completion)))

      ;; Disable defaults, use our settings
      completion-category-defaults nil
      ;; Emacs 31: partial-completion behaves like substring
      completion-pcm-leading-wildcard t)

(vertico-multiform-mode)

;;; § prog-mode's
;;; §§ Version control

;; worktree friendly for e.g. with-editor
(setq git-commit-cd-to-toplevel t)

;;; §§ Project

(use-package b-ripgrep
  :ensure nil
  :commands (b/ripgrep b/ripgrep-project b/ripgrep-main-set-target))

(require 'project)
(bind-key "C-x p r" #'b/ripgrep-project)
(bind-key "C-x p s" #'b/ripgrep-main-set-target)


;;; §§ Emacs Lisp
(defun b/elisp-mode-hook ()
  (setq-local outline-regexp (rx ";;; " (1+ "§"))
              outline-level #'outline-level)
  (outline-minor-mode t))

(add-hook 'emacs-lisp-mode-hook #'b/elisp-mode-hook)

(bind-key "C-<tab>" #'outline-cycle 'emacs-lisp-mode-map)
(bind-key "C-c C-u" #'outline-up-heading)

;;; §§ Nix

(use-package view
  :ensure nil
  :commands (view-mode)
  :bind (:map view-mode-map
	      ("j" . 'next-line)
	      ("k" . 'previous-line)
	      ("n" . 'next-line)
	      ("p" . 'previous-line)))

(defun b/view-mode-for-nix-store ()
  (when (string-prefix-p "/nix/store/" (buffer-file-name))
    (view-mode 1)))

(add-hook 'find-file-hook 'b/view-mode-for-nix-store)

;;; §§ Markdown

(use-package markdown-ts-mode
  :ensure nil
  :mode ("\\.md\\'"))

;;; §§ eglot

;;; The default read-process-output-max is 64KB, which is still quite
;;; conservative. Modern LSP servers like rust-analyzer or clangd
;;; routinely send multi-megabyte responses.
(setq read-process-output-max (* 4 1024 1024)) ; 4MB

(use-package eglot
  :ensure nil
  :bind (:map eglot-mode-map
	      ("<f23>" . eglot-code-actions))
  :commands (eglot)
  :config
  (function-put #'eglot-flymake-backend 'flymake-always-safe t)
  (add-to-list 'eglot-server-programs
             '((rust-ts-mode rust-mode) .
               ("rust-analyzer"
                :initializationOptions
                (:check (:command "clippy")
                        :rust (:analyzerTargetDir "target-lsp"))))))

;;; §§ Rust

(defun b/rust-mode-hook ()
  (electric-pair-local-mode t))

(use-package rust-ts-mode
  :ensure nil
  :mode ("\\.rs\\'". rust-ts-mode)
  :hook (rust-ts-mode . b/rust-mode-hook)
  :config
  (fset 'rust-mode 'rust-ts-mode))

;;; §§ TOML

(use-package toml-ts-mode
  :ensure nil
  :mode "\\.toml\\'")

;;; § Org Mode

(setf org-startup-folded 'show2levels
      org-startup-shrink-all-tables t
      org-cycle-separator-lines 0
      org-fold-catch-invisible-edits 'smart
      org-hide-block-startup t
      org-log-into-drawer "LOGBOOK")

(setf org-todo-keywords
      '((sequence "TODO(t)" "NEXT(n!)" "|" "DONE(d!)")
	(sequence "|" "CNCL(c@)")
	(sequence "WAIT(w!/!)" "|")))
