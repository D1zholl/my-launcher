import getpass
import hashlib
import secrets


def password_hash(password):
    salt = secrets.token_bytes(16)
    digest = hashlib.scrypt(password.encode("utf-8"), salt=salt, n=16384, r=8, p=1, dklen=32)
    return "scrypt$16384$8$1$" + salt.hex() + "$" + digest.hex()


def valid_password(password, confirmation):
    return len(password) >= 8 and password == confirmation


def main():
    password = getpass.getpass("Пароль администратора (минимум 8 символов): ")
    confirmation = getpass.getpass("Повторите пароль: ")
    if not valid_password(password, confirmation):
        raise SystemExit("Пароли должны совпадать и содержать не менее 8 символов.")
    print(password_hash(password))


if __name__ == "__main__":
    main()